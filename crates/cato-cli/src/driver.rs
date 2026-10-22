//! Toolchain driver: target table, toolchain resolution, and exact argv
//! construction for every compiler invocation (spec §1/§2/§5 of the CLI
//! contract). No command string is ever built here — argv arrays only.
//!
//! Composition rules (contract): parsed options first in a fixed order
//! (`-O`, `-g`, `-fsanitize`, `-rtlib`, `--target=`), then all `--`
//! passthrough tokens in order, then driver-owned inputs (`<ll>
//! <runtime> -o <out>`) last — `-o` and the inputs are never overridable.
//! An option present in passthrough (same option key) replaces the parsed
//! value instead of racing it; each option appears exactly once.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Native,
    Wasm,
}

#[derive(Debug, Clone, Copy)]
pub struct TargetSpec {
    pub name: &'static str,
    pub triple: Option<&'static str>,
    pub exe_suffix: &'static str,
    pub flavor: Flavor,
}

pub const TARGETS: &[TargetSpec] = &[
    TargetSpec {
        name: "native",
        triple: None,
        exe_suffix: "",
        flavor: Flavor::Native,
    },
    TargetSpec {
        name: "wasm32",
        triple: Some("wasm32-unknown-unknown"),
        exe_suffix: ".wasm",
        flavor: Flavor::Wasm,
    },
    TargetSpec {
        name: "x86_64-linux",
        triple: Some("x86_64-unknown-linux-gnu"),
        exe_suffix: "",
        flavor: Flavor::Native,
    },
    TargetSpec {
        name: "aarch64-linux",
        triple: Some("aarch64-unknown-linux-gnu"),
        exe_suffix: "",
        flavor: Flavor::Native,
    },
    TargetSpec {
        name: "x86_64-macos",
        triple: Some("x86_64-apple-darwin"),
        exe_suffix: "",
        flavor: Flavor::Native,
    },
    TargetSpec {
        name: "aarch64-macos",
        triple: Some("aarch64-apple-darwin"),
        exe_suffix: "",
        flavor: Flavor::Native,
    },
    TargetSpec {
        name: "x86_64-windows",
        triple: Some("x86_64-w64-mingw32"),
        exe_suffix: ".exe",
        flavor: Flavor::Native,
    },
];

pub fn lookup(name: &str) -> Option<&'static TargetSpec> {
    TARGETS.iter().find(|t| t.name == name)
}

/// The known target names, for the `--target` error message.
pub fn target_names() -> Vec<&'static str> {
    TARGETS.iter().map(|t| t.name).collect()
}

pub fn default_out(spec: &TargetSpec, stem: &str) -> String {
    format!("{stem}{}", spec.exe_suffix)
}

/// The C compiler to link with: `--cc` > `CATO_CC` > `CC` > "clang".
pub fn resolve_cc(flag: Option<&str>) -> String {
    flag.map(|s| s.to_string())
        .or_else(|| std::env::var("CATO_CC").ok())
        .or_else(|| std::env::var("CC").ok())
        .unwrap_or_else(|| "clang".to_string())
}

/// Empty-file probe: can `cc` compile C for `triple`?
/// `<cc> --target=<triple> -c -x c <empty.c> -o <temp>` succeeds.
pub fn cc_supports_triple(cc: &str, triple: &str) -> bool {
    let tmp = std::env::temp_dir().join(format!("cato_probe_{}", std::process::id()));
    let src = tmp.with_extension("c");
    let obj = tmp.with_extension("o");
    let _ = std::fs::write(&src, "");
    let result = std::process::Command::new(cc)
        .arg(format!("--target={triple}"))
        .args(["-c", "-x", "c"])
        .arg(&src)
        .args(["-o"])
        .arg(&obj)
        .output();
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&obj);
    result.map(|o| o.status.success()).unwrap_or(false)
}

pub fn resolve_runtime() -> Result<PathBuf, String> {
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

pub struct LinkOpts {
    pub opt_level: u8,
    pub debug: bool,
    pub asan: bool,
    pub rtlib: String,
    pub passthrough: Vec<String>,
}

pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub what: &'static str,
}

/// The option key of a passthrough token: strip leading `-`s, split at
/// `=`, map `O[0-3]` heads to `"O"`; the head is otherwise the key.
fn passthrough_key(tok: &str) -> String {
    let head = tok.trim_start_matches('-').split('=').next().unwrap_or("");
    if head.len() == 2 && head.starts_with('O') && head.as_bytes()[1].is_ascii_digit() {
        "O".to_string()
    } else {
        head.to_string()
    }
}

fn passthrough_keys(opts: &LinkOpts) -> Vec<String> {
    opts.passthrough
        .iter()
        .map(|t| passthrough_key(t))
        .collect()
}

/// Parsed options in contract order, minus any whose key appears in the
/// passthrough set. `triple` is emitted as `--target=` only when given.
fn parsed_opts(opts: &LinkOpts, triple: Option<&str>) -> Vec<String> {
    let keys = passthrough_keys(opts);
    let mut out = Vec::new();
    if !keys.iter().any(|k| k == "O") {
        out.push(format!("-O{}", opts.opt_level));
    }
    if opts.debug && !keys.iter().any(|k| k == "g") {
        out.push("-g".to_string());
    }
    if opts.asan && !keys.iter().any(|k| k == "fsanitize") {
        out.push("-fsanitize=address".to_string());
    }
    if !keys.iter().any(|k| k == "rtlib") {
        out.push(format!("-rtlib={}", opts.rtlib));
    }
    if let Some(t) = triple
        && !keys.iter().any(|k| k == "target")
    {
        out.push(format!("--target={t}"));
    }
    out
}

/// The effective `-O` flag: parsed level unless the passthrough replaces
/// key `O` (in which case the passthrough token carries its own).
fn effective_opt(opts: &LinkOpts) -> String {
    if let Some(tok) = opts.passthrough.iter().find(|t| passthrough_key(t) == "O") {
        tok.clone()
    } else {
        format!("-O{}", opts.opt_level)
    }
}

/// ELF links need `-lm` explicitly: glibc splits math symbols out of libc,
/// while libSystem and the Windows CRT carry them already.
fn libm_required(triple: Option<&str>) -> bool {
    match triple {
        Some(t) => t.contains("-linux-"),
        None => cfg!(target_os = "linux"),
    }
}

/// The `parallel` runners reach threads through `runtime.c`'s shim, which on
/// POSIX hosts uses pthreads. Recent glibc folded them into libc, but older
/// glibc and musl still want `-lpthread`; naming it is harmless elsewhere.
fn libpthread_required(triple: Option<&str>) -> bool {
    match triple {
        Some(t) => t.contains("-linux-"),
        None => cfg!(target_os = "linux"),
    }
}

pub fn link_invocations(
    spec: &TargetSpec,
    cc: &str,
    ll: &Path,
    runtime: &Path,
    out: &Path,
    opts: &LinkOpts,
) -> Vec<Invocation> {
    let program = cc.to_string();
    match spec.flavor {
        Flavor::Native => {
            let mut args = parsed_opts(opts, spec.triple);
            args.extend(opts.passthrough.iter().cloned());
            args.push(ll.display().to_string());
            args.push(runtime.display().to_string());
            // glibc keeps math symbols in libm; libSystem and the Windows
            // CRT already carry them, so only ELF links need -lm. It sits
            // with the inputs (before -o) so "-o out last" still holds.
            if libm_required(spec.triple) {
                args.push("-lm".to_string());
            }
            if libpthread_required(spec.triple) {
                args.push("-lpthread".to_string());
            }
            args.push("-o".to_string());
            args.push(out.display().to_string());
            vec![Invocation {
                program,
                args,
                what: "link",
            }]
        }
        Flavor::Wasm => {
            let target = spec.triple.unwrap_or("wasm32-unknown-unknown");
            let rt_o = format!("{}.runtime.o", out.display());
            // Passthrough belongs to the link step, not the runtime-object
            // compile step; only the effective -O is shared.
            let rt_args = vec![
                format!("--target={target}"),
                "-c".to_string(),
                effective_opt(opts),
                "-ffreestanding".to_string(),
                runtime.display().to_string(),
                "-o".to_string(),
                rt_o.clone(),
            ];
            let mut link_args = vec![format!("--target={target}"), effective_opt(opts)];
            // -rtlib is a native-link concept; wasm links freestanding
            // (-nostdlib) and never carries it.
            link_args.extend(
                parsed_opts(opts, None)
                    .into_iter()
                    .filter(|a| a != &effective_opt(opts) && !a.starts_with("-rtlib")),
            );
            link_args.extend(opts.passthrough.iter().cloned());
            link_args.push("-nostdlib".to_string());
            link_args.push(ll.display().to_string());
            link_args.push(rt_o);
            link_args.push("-Wl,--no-entry".to_string());
            link_args.push("-Wl,--allow-undefined".to_string());
            link_args.push("-Wl,--export=main".to_string());
            link_args.push("-o".to_string());
            link_args.push(out.display().to_string());
            vec![
                Invocation {
                    program: program.clone(),
                    args: rt_args,
                    what: "wasm runtime",
                },
                Invocation {
                    program,
                    args: link_args,
                    what: "wasm link",
                },
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> LinkOpts {
        LinkOpts {
            opt_level: 2,
            debug: false,
            asan: false,
            rtlib: "compiler-rt".into(),
            passthrough: vec![],
        }
    }

    #[test]
    fn native_link_argv_baseline() {
        let spec = lookup("native").unwrap();
        let o = opts();
        let v = link_invocations(
            spec,
            "clang",
            Path::new("a.ll"),
            Path::new("runtime/runtime.c"),
            Path::new("a"),
            &o,
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].program, "clang");
        assert_eq!(v[0].what, "link");
        let mut expected = vec!["-O2", "-rtlib=compiler-rt", "a.ll", "runtime/runtime.c"];
        if cfg!(target_os = "linux") {
            expected.push("-lm");
            expected.push("-lpthread");
        }
        expected.extend(["-o", "a"]);
        assert_eq!(v[0].args, expected);
    }

    #[test]
    fn linux_triples_link_libm() {
        let spec = lookup("x86_64-linux").unwrap();
        let v = link_invocations(
            spec,
            "clang",
            Path::new("a.ll"),
            Path::new("rt.c"),
            Path::new("a"),
            &opts(),
        );
        assert!(
            v[0].args.contains(&"-lm".to_string()),
            "args: {:?}",
            v[0].args
        );
        assert!(
            v[0].args.contains(&"-lpthread".to_string()),
            "args: {:?}",
            v[0].args
        );
    }

    #[test]
    fn passthrough_rtlib_replaces_parsed_default() {
        let spec = lookup("native").unwrap();
        let mut o = opts();
        o.passthrough = vec!["--rtlib=libgcc".into()];
        let v = link_invocations(
            spec,
            "clang",
            Path::new("a.ll"),
            Path::new("rt.c"),
            Path::new("a"),
            &o,
        );
        let args = &v[0].args;
        assert!(
            args.contains(&"--rtlib=libgcc".to_string()),
            "args: {args:?}"
        );
        assert!(
            !args.contains(&"-rtlib=compiler-rt".to_string()),
            "args: {args:?}"
        );
    }

    #[test]
    fn passthrough_opt_level_replaces_parsed() {
        let spec = lookup("native").unwrap();
        let mut o = opts();
        o.passthrough = vec!["-O3".into()];
        let v = link_invocations(
            spec,
            "clang",
            Path::new("a.ll"),
            Path::new("rt.c"),
            Path::new("a"),
            &o,
        );
        let args = &v[0].args;
        assert!(args.contains(&"-O3".to_string()), "args: {args:?}");
        assert!(!args.contains(&"-O2".to_string()), "args: {args:?}");
    }

    #[test]
    fn debug_and_asan_flags_emit_g_and_sanitize() {
        let spec = lookup("native").unwrap();
        let mut o = opts();
        o.debug = true;
        o.asan = true;
        let v = link_invocations(
            spec,
            "clang",
            Path::new("a.ll"),
            Path::new("rt.c"),
            Path::new("a"),
            &o,
        );
        let args = &v[0].args;
        assert!(args.contains(&"-g".to_string()), "args: {args:?}");
        assert!(
            args.contains(&"-fsanitize=address".to_string()),
            "args: {args:?}"
        );
    }

    #[test]
    fn wasm_produces_two_invocations() {
        let spec = lookup("wasm32").unwrap();
        let o = opts();
        let v = link_invocations(
            spec,
            "clang",
            Path::new("a.ll"),
            Path::new("runtime/runtime.c"),
            Path::new("out"),
            &o,
        );
        assert_eq!(v.len(), 2);
        let rt_o_str = "out.runtime.o".to_string();
        assert_eq!(v[0].what, "wasm runtime");
        assert_eq!(
            v[0].args,
            [
                "--target=wasm32-unknown-unknown",
                "-c",
                "-O2",
                "-ffreestanding",
                "runtime/runtime.c",
                "-o",
                &rt_o_str
            ]
        );
        assert_eq!(v[1].what, "wasm link");
        assert_eq!(
            v[1].args,
            [
                "--target=wasm32-unknown-unknown",
                "-O2",
                "-nostdlib",
                "a.ll",
                &rt_o_str,
                "-Wl,--no-entry",
                "-Wl,--allow-undefined",
                "-Wl,--export=main",
                "-o",
                "out"
            ]
        );
    }

    #[test]
    fn default_out_appends_target_suffix() {
        let native = lookup("native").unwrap();
        assert_eq!(default_out(native, "foo"), "foo");
        let wasm = lookup("wasm32").unwrap();
        assert_eq!(default_out(wasm, "foo"), "foo.wasm");
    }

    #[test]
    fn resolve_cc_flag_beats_env() {
        assert_eq!(resolve_cc(Some("mycc")), "mycc");
    }

    #[test]
    fn resolve_runtime_missing_cat_runtime_errors() {
        let old = std::env::var("CATO_RUNTIME").ok();
        // SAFETY: `CATO_RUNTIME` is process-global, and this is the only
        // test in the binary that reads or writes it, so the mutation
        // cannot race another thread.
        unsafe {
            std::env::set_var("CATO_RUNTIME", "/definitely/missing/runtime.c");
        }
        let r = resolve_runtime();
        // SAFETY: restore the value read before the mutation above.
        unsafe {
            match old {
                Some(v) => std::env::set_var("CATO_RUNTIME", v),
                None => std::env::remove_var("CATO_RUNTIME"),
            }
        }
        let err = r.expect_err("missing CATO_RUNTIME must error");
        assert!(err.contains("does not exist"), "err: {err}");
    }

    #[test]
    fn target_table_matches_spec() {
        let expect: &[(&str, Option<&str>, &str, Flavor)] = &[
            ("native", None, "", Flavor::Native),
            (
                "wasm32",
                Some("wasm32-unknown-unknown"),
                ".wasm",
                Flavor::Wasm,
            ),
            (
                "x86_64-linux",
                Some("x86_64-unknown-linux-gnu"),
                "",
                Flavor::Native,
            ),
            (
                "aarch64-linux",
                Some("aarch64-unknown-linux-gnu"),
                "",
                Flavor::Native,
            ),
            (
                "x86_64-macos",
                Some("x86_64-apple-darwin"),
                "",
                Flavor::Native,
            ),
            (
                "aarch64-macos",
                Some("aarch64-apple-darwin"),
                "",
                Flavor::Native,
            ),
            (
                "x86_64-windows",
                Some("x86_64-w64-mingw32"),
                ".exe",
                Flavor::Native,
            ),
        ];
        assert_eq!(TARGETS.len(), expect.len(), "TARGETS: {TARGETS:?}");
        for (spec, (name, triple, suffix, flavor)) in TARGETS.iter().zip(expect) {
            assert_eq!(spec.name, *name);
            assert_eq!(spec.triple, *triple, "{} triple", spec.name);
            assert_eq!(spec.exe_suffix, *suffix, "{} suffix", spec.name);
            assert_eq!(spec.flavor, *flavor, "{} flavor", spec.name);
        }
    }

    #[test]
    fn windows_default_out_has_exe_suffix() {
        let win = lookup("x86_64-windows").unwrap();
        assert_eq!(default_out(win, "foo"), "foo.exe");
    }
}
