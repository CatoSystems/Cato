//! Native and WASM builds: emit LLVM IR, then link it with the C runtime
//! into an executable via the system C compiler (native) or a
//! wasm-capable clang plus lld (wasm32).
//!
//! The runtime is located relative to the compiler binary (the repo dev
//! layout `target/debug/cato` → `<root>/runtime/runtime.c`), the working
//! directory, or `CATO_RUNTIME`.

use crate::cli::{Cli, Target};
use cato_core::{Backend, Program};
use cato_llvm::{LlvmBackend, Target as LlvmTarget};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The C compiler to link with: `--cc` > `CATO_CC` > `CC` > "clang".
fn resolve_cc(cli: &Cli) -> String {
    cli.cc
        .clone()
        .or_else(|| std::env::var("CATO_CC").ok())
        .or_else(|| std::env::var("CC").ok())
        .unwrap_or_else(|| "clang".to_string())
}

fn resolve_runtime() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CATO_RUNTIME") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!("CATO_RUNTIME={} does not exist", p.display()));
    }
    // Dev/install layouts: walk up from the compiler binary looking for a
    // `runtime/runtime.c` sibling (target/debug/cato -> repo root).
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let cand = d.join("runtime").join("runtime.c");
            if cand.exists() {
                return Ok(cand);
            }
            dir = d.parent();
        }
    }
    let cand = Path::new("runtime").join("runtime.c");
    if cand.exists() {
        return Ok(cand);
    }
    Err(
        "cannot locate runtime/runtime.c (tried $CATO_RUNTIME, the compiler's \
         install layout, and ./runtime) — set CATO_RUNTIME to the file's path"
            .to_string(),
    )
}

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
/// wasm (fail loudly if it does not), otherwise prefer Homebrew LLVM, then
/// the system compiler.
fn resolve_wasm_cc(cli: &Cli) -> Result<String, String> {
    let guidance = |why: &str| {
        format!(
            "{why}\n\
             wasm32 codegen itself always works: 'cato <file> --emit-ir --target wasm32'\n\
             to LINK .wasm binaries install a wasm-capable clang with lld, e.g.\n\
             \x20 brew install llvm lld\n\
             \x20 cato build --target wasm32 <file> --cc $(brew --prefix llvm)/bin/clang"
        )
    };
    if let Some(cc) = &cli.cc {
        if !cc_supports_wasm32(cc) {
            return Err(guidance(&format!(
                "cato: --cc {cc} does not support the wasm32 target"
            )));
        }
        if !cc_has_wasm_ld(cc) {
            return Err(guidance(&format!(
                "cato: --cc {cc} has no wasm-ld available (install lld)"
            )));
        }
        return Ok(cc.clone());
    }
    for cc in ["/opt/homebrew/opt/llvm/bin/clang", "/usr/local/opt/llvm/bin/clang", "clang"] {
        if cc_supports_wasm32(cc) && cc_has_wasm_ld(cc) {
            return Ok(cc.to_string());
        }
    }
    Err(guidance("cato: no wasm32-capable compiler with wasm-ld found"))
}

/// Emit IR for `program` and link it into an executable at `out`.
/// The intermediate IR goes to `cli.emit_ll` when given (and is kept),
/// otherwise a temp file that is deleted after linking.
pub fn build_native(program: &Program, cli: &Cli, out: &Path) -> Result<PathBuf, String> {
    let backend = LlvmBackend {
        fast: cli.fast,
        threads: cli.threads,
        target: match cli.target {
            Target::Native => LlvmTarget::Native,
            Target::Wasm32 => LlvmTarget::Wasm32,
        },
    };
    let ir = backend
        .emit(program)
        .map_err(|e| format!("backend error: {}", e))?;

    let keep_ll = cli.emit_ll.clone();
    let ll_path: PathBuf = keep_ll
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| unique_temp("build", ".ll"));
    std::fs::write(&ll_path, &ir)
        .map_err(|e| format!("cannot write {}: {}", ll_path.display(), e))?;

    let result = match cli.target {
        Target::Native => link_native(&ll_path, cli, out),
        Target::Wasm32 => link_wasm(&ll_path, cli, out),
    };
    if keep_ll.is_none() {
        let _ = std::fs::remove_file(&ll_path);
    }
    result.map(|_| out.to_path_buf())
}

fn run_cc(mut cmd: Command, what: &str) -> Result<(), String> {
    let result = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {}", what, e))?;
    if !result.status.success() {
        return Err(format!(
            "{} failed (exit {}):\n{}",
            what,
            result.status,
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(())
}

fn link_native(ll_path: &Path, cli: &Cli, out: &Path) -> Result<(), String> {
    let rt = resolve_runtime()?;
    let cc = resolve_cc(cli);
    let mut cmd = Command::new(&cc);
    cmd.arg(format!("-O{}", cli.opt_level));
    if cli.asan {
        cmd.arg("-fsanitize=address");
    }
    cmd.arg(ll_path).arg(&rt).arg("-o").arg(out);
    run_cc(cmd, &cc)
}

/// Link a wasm32 module: compile the runtime C helpers for the same
/// target, then link freestanding with `--no-entry` (print/malloc become
/// host imports thanks to `--allow-undefined`).
fn link_wasm(ll_path: &Path, cli: &Cli, out: &Path) -> Result<(), String> {
    let cc = resolve_wasm_cc(cli)?;
    let rt = resolve_runtime()?;

    let rt_o = unique_temp("runtime", ".o");
    let mut compile = Command::new(&cc);
    compile
        .arg("--target=wasm32-unknown-unknown")
        .args(["-c", "-O2", "-ffreestanding"])
        .arg(&rt)
        .arg("-o")
        .arg(&rt_o);
    let compile_what = format!("{cc} (runtime → wasm object)");
    let compiled = run_cc(compile, &compile_what);
    if compiled.is_err() {
        let _ = std::fs::remove_file(&rt_o);
        return compiled;
    }

    let mut link = Command::new(&cc);
    link.arg("--target=wasm32-unknown-unknown")
        .arg(format!("-O{}", cli.opt_level))
        .arg("-nostdlib")
        .arg(ll_path)
        .arg(&rt_o)
        .args([
            "-Wl,--no-entry",
            "-Wl,--allow-undefined",
            "-Wl,--export=main",
        ])
        .arg("-o")
        .arg(out);
    let linked = run_cc(link, &format!("{cc} (wasm link)"));
    let _ = std::fs::remove_file(&rt_o);
    linked
}

/// Execute a built binary with inherited stdio; returns its exit code.
pub fn run_binary(bin: &Path) -> Result<i32, String> {
    let status = Command::new(bin)
        .status()
        .map_err(|e| format!("cannot run {}: {}", bin.display(), e))?;
    Ok(status.code().unwrap_or(1))
}
