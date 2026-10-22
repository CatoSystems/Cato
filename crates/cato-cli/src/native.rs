//! Native and WASM builds: emit LLVM IR, then link it with the C runtime
//! into an executable via the system C compiler (native) or a
//! wasm-capable clang plus lld (wasm32).
//!
//! The runtime is located relative to the compiler binary (the repo dev
//! layout `target/debug/cato` → `<root>/runtime/runtime.c`), the working
//! directory, or `CATO_RUNTIME`.

use crate::cli::Cli;
use crate::driver::Flavor;
use cato_core::{Backend, Program};
use cato_llvm::{LlvmBackend, Target as LlvmTarget};
use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_temp(tag: &str, ext: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("cato_{}_{}_{}{}", std::process::id(), n, tag, ext))
}

/// Does this compiler support the wasm32 target?
fn cc_supports_wasm32(cc: &str) -> bool {
    Command::new(cc)
        .arg("--print-targets")
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.starts_with("    wasm32"))
        })
        .unwrap_or(false)
}

/// Can this compiler reach a wasm linker? `-print-file-name` echoes the
/// bare name back when the file cannot be found.
fn cc_has_wasm_ld(cc: &str) -> bool {
    Command::new(cc)
        .args(["-print-file-name=wasm-ld"])
        .output()
        .map(|out| {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            s != "wasm-ld" && Path::new(s).exists()
        })
        .unwrap_or(false)
}

/// Pick a wasm-capable compiler: an explicit `--cc` is required to support
/// wasm (fail loudly if it does not), otherwise prefer Homebrew LLVM (macOS),
/// then the system compiler.
fn resolve_wasm_cc(cli: &Cli) -> Result<String, String> {
    let guidance = |why: &str| {
        format!(
            "{why}\n\
             wasm32 codegen itself always works: 'cato compile <file> --emit-ir --target wasm32'\n\
             to LINK .wasm binaries install a wasm-capable clang with lld:\n\
             \x20 brew install llvm lld\n\
             \x20 cato compile --target wasm32 <file> --cc $(brew --prefix llvm)/bin/clang"
        )
    };
    if let Some(cc) = &cli.cc {
        if !cc_supports_wasm32(cc) {
            return Err(guidance(&format!(
                "--cc {cc} does not support the wasm32 target"
            )));
        }
        if !cc_has_wasm_ld(cc) {
            return Err(guidance(&format!(
                "--cc {cc} has no wasm-ld available (install lld)"
            )));
        }
        return Ok(cc.clone());
    }
    #[cfg(windows)]
    let candidates: &[&str] = &[r"C:\Program Files\LLVM\bin\clang.exe", "clang"];
    #[cfg(not(windows))]
    let candidates: &[&str] = &[
        "/opt/homebrew/opt/llvm/bin/clang",
        "/usr/local/opt/llvm/bin/clang",
        "clang",
    ];
    for cc in candidates {
        if cc_supports_wasm32(cc) && cc_has_wasm_ld(cc) {
            return Ok(cc.to_string());
        }
    }
    Err(guidance("no wasm32-capable compiler with wasm-ld found"))
}

/// Emit IR for `program` and link it into an executable at `out`.
/// The intermediate IR goes to `cli.emit_ll` when given (and is kept),
/// otherwise a temp file that is deleted after linking. All toolchain
/// argv is assembled by `driver::link_invocations`.
pub fn build_native(program: &Program, cli: &Cli, out: &Path) -> Result<PathBuf, String> {
    #[cfg(feature = "static-llvm")]
    {
        if cli.target.flavor == Flavor::Native {
            return crate::hermetic::build_hermetic(program, cli, out);
        }
    }
    let backend = LlvmBackend {
        fast: cli.fast,
        threads: cli.threads,
        target: match (cli.target.flavor, cli.target.triple) {
            (Flavor::Wasm, _) => LlvmTarget::Wasm32,
            (Flavor::Native, Some(t)) => LlvmTarget::Triple(t),
            (Flavor::Native, None) => LlvmTarget::Native,
        },
    };
    let ir = backend
        .emit(program)
        .map_err(|e| format!("backend error: {}", e))?;
    let ir = if cli.asan {
        cato_llvm::with_function_attribute(&ir, "sanitize_address")
    } else {
        ir
    };

    let keep_ll = cli.emit_ll.clone();
    let ll_path: PathBuf = keep_ll
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| unique_temp("build", ".ll"));
    std::fs::write(&ll_path, &ir)
        .map_err(|e| format!("cannot write {}: {}", ll_path.display(), e))?;

    let result = link(&ll_path, cli, out);
    if keep_ll.is_none() {
        let _ = std::fs::remove_file(&ll_path);
    }
    result.map(|_| out.to_path_buf())
}

/// `exited with status 1` / `was killed by signal 9`.
pub(crate) fn describe_status(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exited with status {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("was killed by signal {sig}");
        }
    }
    status.to_string()
}

fn run_cc(program: &str, args: &[String], what: &str) -> Result<(), String> {
    let result = Command::new(program).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "cannot run C compiler '{program}' for the {what} step: not found \
                 (pass --cc <path> or set CATO_CC)"
            )
        } else {
            format!("cannot run C compiler '{program}' for the {what} step: {e}")
        }
    })?;
    // Some platforms report a missing program as a successful spawn that
    // exits 127 (command not found) or 126 (not executable) instead of an
    // NotFound io error, so the actionable message must fire on the status,
    // not only on the error.
    if matches!(result.status.code(), Some(126) | Some(127)) {
        return Err(format!(
            "cannot run C compiler '{program}' for the {what} step: not found \
             (pass --cc <path> or set CATO_CC)"
        ));
    }
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stderr = stderr.trim_end();
        let mut msg = format!(
            "{what} failed: {program} {}",
            describe_status(result.status)
        );
        if !stderr.is_empty() {
            msg.push_str(":\n");
            msg.push_str(stderr);
        }
        return Err(msg);
    }
    Ok(())
}

/// The runtime source: `--runtime` (must exist) or the repo/env lookup.
fn resolve_runtime_path(cli: &Cli) -> Result<PathBuf, String> {
    match &cli.runtime {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.exists() {
                Ok(p)
            } else {
                Err(format!("--runtime={} does not exist", p.display()))
            }
        }
        None => crate::driver::resolve_runtime(),
    }
}

/// Link `ll_path` at `out` through `driver::link_invocations` — every
/// toolchain argv in the compiler comes from there (spec §5).
fn link(ll_path: &Path, cli: &Cli, out: &Path) -> Result<(), String> {
    let cc = match cli.target.flavor {
        Flavor::Native => crate::driver::resolve_cc(cli.cc.as_deref()),
        Flavor::Wasm => resolve_wasm_cc(cli)?,
    };
    // Native cross targets probe the toolchain before any link argv runs
    // (spec §2: a valid name with an incapable cc is a tool failure).
    if cli.target.flavor == Flavor::Native {
        if let Some(triple) = cli.target.triple {
            if !crate::driver::cc_supports_triple(&cc, triple) {
                let names = crate::driver::target_names().join(", ");
                return Err(format!(
                    "{cc} does not support target {triple} (known: {names})"
                ));
            }
        }
    }
    let rt = resolve_runtime_path(cli)?;
    let opts = crate::driver::LinkOpts {
        opt_level: cli.opt_level,
        debug: cli.debug,
        asan: cli.asan,
        rtlib: cli.rtlib.clone(),
        passthrough: cli.passthrough.clone(),
    };
    let invs = crate::driver::link_invocations(cli.target, &cc, ll_path, &rt, out, &opts);
    for inv in &invs {
        run_cc(&inv.program, &inv.args, inv.what)?;
    }
    // The wasm link step leaves its runtime object next to the output.
    if cli.target.flavor == Flavor::Wasm {
        let _ = std::fs::remove_file(format!("{}.runtime.o", out.display()));
    }
    Ok(())
}

/// Emit IR for `program`, honouring the CLI's target selection. Used by the
/// hermetic build, which links the IR itself instead of invoking `cc`.
#[cfg(feature = "static-llvm")]
pub fn emit_llvm_ir(program: &Program, cli: &Cli) -> Result<String, String> {
    let backend = LlvmBackend {
        fast: cli.fast,
        threads: cli.threads,
        target: match (cli.target.flavor, cli.target.triple) {
            (Flavor::Wasm, _) => LlvmTarget::Wasm32,
            (Flavor::Native, Some(t)) => LlvmTarget::Triple(t),
            (Flavor::Native, None) => LlvmTarget::Native,
        },
    };
    backend
        .emit(program)
        .map_err(|e| format!("backend error: {}", e))
}
