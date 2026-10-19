#[cfg(feature = "static-llvm")]
use crate::cli::Cli;
#[cfg(feature = "static-llvm")]
use crate::driver::{Flavor, TargetSpec};
#[cfg(feature = "static-llvm")]
use cato_core::Program;
#[cfg(feature = "static-llvm")]
use crate::objgen;
#[cfg(feature = "static-llvm")]
use cato_assets::{assets_for, host_target_name};
#[cfg(feature = "static-llvm")]
use std::ffi::{CStr, CString};
#[cfg(feature = "static-llvm")]
use std::fs;
#[cfg(feature = "static-llvm")]
use std::path::{Path, PathBuf};

#[cfg(feature = "static-llvm")]
#[derive(Debug)]
struct TempDir {
    path: PathBuf,
}

#[cfg(feature = "static-llvm")]
impl TempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let mut base = std::env::temp_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let pid = std::process::id();
        let name = format!("{}_{}_{}", prefix, pid, ts);
        base.push(name);
        fs::create_dir_all(&base)?;
        Ok(TempDir { path: base })
    }
}

#[cfg(feature = "static-llvm")]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(feature = "static-llvm")]
fn unique_temp(prefix: &str) -> std::io::Result<PathBuf> {
    TempDir::new(prefix).map(|t| {
        let p = t.path.clone();
        Box::leak(Box::new(t));
        p
    })
}

#[cfg(feature = "static-llvm")]
fn reject_unsupported(cli: &Cli) -> Result<(), String> {
    if let Some(ref cc) = cli.cc {
        return Err(format!(
            "cato: --cc '{}' is not supported with hermetic static linking (remove --cc)",
            cc
        ));
    }
    if cli.runtime.is_some() {
        return Err(
            "cato: --runtime is not supported with hermetic static linking (remove --runtime)"
                .to_string(),
        );
    }
    if cli.asan {
        return Err(
            "cato: --asan is not supported with hermetic static linking (remove --asan)"
                .to_string(),
        );
    }
    if cli.rtlib_given {
        return Err(
            "cato: --rtlib is not supported with hermetic static linking (remove --rtlib)"
                .to_string(),
        );
    }
    if !cli.passthrough.is_empty() {
        let first = cli.passthrough.first().cloned().unwrap_or_default();
        return Err(format!(
            "cato: passthrough flags are not supported with hermetic static linking ('{}')",
            first
        ));
    }
    Ok(())
}

#[cfg(feature = "static-llvm")]
fn llvm_triple_for_target(target_name: &str) -> &str {
    match target_name {
        "x86_64-linux" => "x86_64-unknown-linux-musl",
        "aarch64-linux" => "aarch64-unknown-linux-musl",
        "x86_64-windows" => "x86_64-pc-windows-gnu",
        "x86_64-macos" => "x86_64-apple-darwin",
        "aarch64-macos" => "aarch64-apple-darwin",
        _ => target_name,
    }
}

#[cfg(feature = "static-llvm")]
fn resolve_target_asset_name(spec: &TargetSpec) -> Result<String, String> {
    if spec.name == "native" {
        host_target_name()
            .map(|s| s.to_string())
            .ok_or_else(|| "cato: hermetic compile is not supported on this host for --target native".to_string())
    } else {
        Ok(spec.name.to_string())
    }
}

#[cfg(feature = "static-llvm")]
pub(super) fn hermetic_link_argv(
    target_name: &str,
    staged: &Path,
    out: &Path,
) -> Result<(cato_lld_sys::Flavor, Vec<String>), String> {
    let out_str = out.to_string_lossy().to_string();
    let staged_str = staged.to_string_lossy().to_string();
    let _assets = assets_for(target_name)?;
    match target_name {
        "x86_64-linux" => Ok((
            cato_lld_sys::Flavor::Elf,
            vec![
                "ld.lld".to_string(),
                "-static".to_string(),
                "-m".to_string(),
                "elf_x86_64".to_string(),
                "-o".to_string(),
                out_str,
                format!("{}/crt1.o", staged_str),
                format!("{}/crti.o", staged_str),
                format!("{}/prog.o", staged_str),
                format!("{}/runtime.o", staged_str),
                "-L".to_string(),
                staged_str.clone(),
                "-lc".to_string(),
                "-lzigc".to_string(),
                "-lcompiler_rt".to_string(),
                format!("{}/crtn.o", staged_str),
            ],
        )),
        "aarch64-linux" => Ok((
            cato_lld_sys::Flavor::Elf,
            vec![
                "ld.lld".to_string(),
                "-static".to_string(),
                "-o".to_string(),
                out_str,
                format!("{}/crt1.o", staged_str),
                format!("{}/crti.o", staged_str),
                format!("{}/prog.o", staged_str),
                format!("{}/runtime.o", staged_str),
                "-L".to_string(),
                staged_str.clone(),
                "-lc".to_string(),
                "-lzigc".to_string(),
                "-lcompiler_rt".to_string(),
                format!("{}/crtn.o", staged_str),
            ],
        )),
        "x86_64-windows" => {
            let assets = assets_for(target_name).map_err(|e| e)?;
            let mut args = vec![
                "ld.lld".to_string(),
                "-m".to_string(),
                "i386pep".to_string(),
                "--subsystem".to_string(),
                "console".to_string(),
                "-o".to_string(),
                out_str,
                format!("{}/crt2.o", staged_str),
                format!("{}/prog.o", staged_str),
                format!("{}/runtime.o", staged_str),
                "-L".to_string(),
                staged_str.clone(),
            ];
            for (name, _) in assets.files {
                match name.as_ref() {
                    "crt2.o" | "prog.o" | "runtime.o" => {}
                    _ => args.push(format!("{}/{}", staged_str, name)),
                }
            }
            Ok((cato_lld_sys::Flavor::Mingw, args))
        }
        "x86_64-macos" => Ok((
            cato_lld_sys::Flavor::Darwin,
            vec![
                "ld64.lld".to_string(),
                "-arch".to_string(),
                "x86_64".to_string(),
                "-platform_version".to_string(),
                "macos".to_string(),
                "11.0".to_string(),
                "11.0".to_string(),
                "-o".to_string(),
                out_str,
                format!("{}/prog.o", staged_str),
                format!("{}/runtime.o", staged_str),
                "-L".to_string(),
                staged_str,
                "-lSystem".to_string(),
            ],
        )),
        "aarch64-macos" => Ok((
            cato_lld_sys::Flavor::Darwin,
            vec![
                "ld64.lld".to_string(),
                "-arch".to_string(),
                "arm64".to_string(),
                "-platform_version".to_string(),
                "macos".to_string(),
                "11.0".to_string(),
                "11.0".to_string(),
                "-o".to_string(),
                out_str,
                format!("{}/prog.o", staged_str),
                format!("{}/runtime.o", staged_str),
                "-L".to_string(),
                staged_str,
                "-lSystem".to_string(),
            ],
        )),
        _ => {
            let _ = assets_for(target_name);
            Err(format!(
                "cato: no embedded assets for target '{}{}'",
                target_name,
                if target_name == "wasm32" {
                    " (this cato build does not embed wasm32)"
                } else {
                    ""
                }
            ))
        }
    }
}

#[cfg(feature = "static-llvm")]
pub fn build_hermetic(
    program: &Program,
    cli: &Cli,
    out: &Path,
) -> Result<PathBuf, String> {
    reject_unsupported(cli)?;
    let target_name = resolve_target_asset_name(&cli.target)?;
    let ir = crate::native::emit_llvm_ir(program, cli).map_err(|e| e.to_string())?;
    if let Some(ref ll_path) = cli.emit_ll {
        fs::write(ll_path, &ir).map_err(|e| format!("cato: cannot write LLVM IR: {}", e))?;
    }
    let obj = objgen::emit_object(&ir, llvm_triple_for_target(&target_name), cli.opt_level)?;
    let staged = unique_temp("cato-hermetic")
        .map_err(|e| format!("cato: cannot create staging dir: {}", e))?;
    let prog_o = staged.join("prog.o");
    fs::write(&prog_o, &obj).map_err(|e| format!("cato: cannot write object: {}", e))?;
    if let Ok(assets) = assets_for(&target_name) {
        for (name, bytes) in assets.files {
            let dst = staged.join(name);
            fs::write(&dst, bytes).map_err(|e| format!("cato: cannot stage asset: {}", e))?;
        }
    }
    let (flavor, argv) = hermetic_link_argv(&target_name, &staged, out)?;
    #[cfg(feature = "in-process-lld")]
    {
        let mut cstrs: Vec<CString> = Vec::with_capacity(argv.len());
        for a in &argv {
            cstrs.push(CString::new(a.as_str()).map_err(|_| "cato: invalid argv string")?);
        }
        let refs: Vec<&CStr> = cstrs.iter().map(|c| c.as_c_str()).collect();
        cato_lld_sys::link(flavor, &refs).map_err(|e| format!("cato: link failed: {}", e))?;
        Ok(out.to_path_buf())
    }
    #[cfg(not(feature = "in-process-lld"))]
    {
        let _ = (flavor, argv);
        Err("cato: hermetic linking unavailable in this build".to_string())
    }
}



#[cfg(all(feature = "static-llvm", test))]
mod tests {
    use super::*;
    use crate::driver::lookup;

    #[test]
    fn rejects_cc_runtime_asan_rtlib_passthrough() {
        let mut cli = Cli::default();
        let mut c = cli.clone();
        c.cc = Some("/bin/cc".to_string());
        assert!(reject_unsupported(&c).is_err());
        let mut c = cli.clone();
        c.runtime = Some("runtime/runtime.c".to_string());
        assert!(reject_unsupported(&c).is_err());
        let mut c = cli.clone();
        c.asan = true;
        assert!(reject_unsupported(&c).is_err());
        let mut c = cli.clone();
        c.rtlib_given = true;
        assert!(reject_unsupported(&c).is_err());
        let mut c = cli.clone();
        c.passthrough = vec!["-Wl,--no-as-needed".to_string()];
        assert!(reject_unsupported(&c).is_err());
    }

    #[test]
    fn linux_argv_shape() {
        let t = std::env::temp_dir().join("cato-test");
        let out = t.join("out");
        let (flavor, argv) = hermetic_link_argv("x86_64-linux", &t, &out).unwrap();
        assert!(matches!(flavor, cato_lld_sys::Flavor::Elf));
        assert_eq!(argv[0], "ld.lld");
        assert_eq!(argv[1], "-static");
        assert_eq!(argv[2], "-m");
        assert_eq!(argv[3], "elf_x86_64");
        assert_eq!(argv[4], "-o");
        assert!(argv.contains(&format!("{}/prog.o", t.display())));
        assert!(argv.contains(&format!("{}/runtime.o", t.display())));
        assert!(argv.contains(&"-lc".to_string()));
        assert!(argv.contains(&"-lzigc".to_string()));
    }

    #[test]
    fn aarch64_linux_argv_selects_no_m() {
        let t = std::env::temp_dir().join("cato-test2");
        let out = t.join("out");
        let (flavor, argv) = hermetic_link_argv("aarch64-linux", &t, &out).unwrap();
        assert!(matches!(flavor, cato_lld_sys::Flavor::Elf));
        assert_eq!(argv[0], "ld.lld");
        assert!(!argv.contains(&"-m".to_string()));
    }

    #[test]
    fn windows_argv_shape() {
        let t = std::env::temp_dir().join("cato-test3");
        let out = t.join("out.exe");
        let (flavor, argv) = hermetic_link_argv("x86_64-windows", &t, &out).unwrap();
        assert!(matches!(flavor, cato_lld_sys::Flavor::Mingw));
        assert!(argv.iter().any(|a| a == "-m"));
        assert!(argv.iter().any(|a| a == "i386pep"));
        assert!(argv.iter().any(|a| a == "--subsystem"));
        assert!(argv.iter().any(|a| a == "console"));
    }

    #[test]
    fn macos_argv_shape_x86_64() {
        let t = std::env::temp_dir().join("cato-test4");
        let out = t.join("out");
        let (flavor, argv) = hermetic_link_argv("x86_64-macos", &t, &out).unwrap();
        assert!(matches!(flavor, cato_lld_sys::Flavor::Darwin));
        assert!(argv.contains(&"-arch".to_string()));
        assert!(argv.contains(&"x86_64".to_string()));
        assert!(argv.contains(&"-platform_version".to_string()));
        assert!(argv.contains(&"-lSystem".to_string()));
    }

    #[test]
    fn macos_argv_shape_aarch64_uses_arm64() {
        let t = std::env::temp_dir().join("cato-test5");
        let out = t.join("out");
        let (flavor, argv) = hermetic_link_argv("aarch64-macos", &t, &out).unwrap();
        assert!(matches!(flavor, cato_lld_sys::Flavor::Darwin));
        assert!(argv.contains(&"arm64".to_string()));
    }

    #[test]
    fn unknown_asset_target_errors() {
        let t = std::env::temp_dir();
        let out = t.join("out");
        let e = hermetic_link_argv("wasm32", &t, &out).expect_err("should fail");
        assert!(e.contains("no embedded assets for target 'wasm32'"));
        assert!(e.contains("this cato build does not embed wasm32"));
    }
}
