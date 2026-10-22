#[cfg(feature = "static-llvm")]
use crate::cli::Cli;
#[cfg(feature = "static-llvm")]
use crate::driver::TargetSpec;
#[cfg(feature = "static-llvm")]
use crate::objgen;
#[cfg(feature = "static-llvm")]
use cato_assets::{assets_for, host_target_name};
#[cfg(feature = "static-llvm")]
use cato_core::Program;
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
fn unique_temp(prefix: &str) -> std::io::Result<TempDir> {
    TempDir::new(prefix)
}

#[cfg(feature = "static-llvm")]
fn reject_unsupported(cli: &Cli) -> Result<(), String> {
    if cli.cc.is_some() {
        return Err(
            "cato: --cc is not used in this cato build (it links hermetically; no external C compiler is consulted)"
                .to_string(),
        );
    }
    if cli.runtime.is_some() {
        return Err(
            "cato: --runtime is not supported in this cato build (the runtime is embedded)"
                .to_string(),
        );
    }
    if cli.asan {
        return Err("cato: --asan is not supported in this cato build".to_string());
    }
    if cli.debug {
        return Err(
            "cato: -g is not supported in this cato build (in-process codegen emits no debug info)"
                .to_string(),
        );
    }
    if cli.rtlib_given {
        return Err(
            "cato: --rtlib is not supported in this cato build (compiler-rt is embedded)"
                .to_string(),
        );
    }
    if !cli.passthrough.is_empty() {
        let first = cli.passthrough.first().cloned().unwrap_or_default();
        return Err(format!(
            "cato: passthrough '{}' is not supported in this cato build",
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
        host_target_name().map(|s| s.to_string()).ok_or_else(|| {
            "cato: hermetic compile is not supported on this host for --target native".to_string()
        })
    } else {
        Ok(spec.name.to_string())
    }
}

#[cfg(feature = "static-llvm")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LldFlavor {
    Elf,
    Mingw,
    Darwin,
}

/// The `-flavor` value that selects the driver lld would pick from argv[0].
///
/// The bundled lld is spawned as a subprocess, and a subprocess cannot
/// control its own argv[0] on every platform (Windows has no equivalent of
/// exec's argv[0] override), so the driver is selected explicitly instead.
#[cfg(cato_bundled_lld)]
fn lld_flavor_option(flavor: LldFlavor) -> &'static str {
    match flavor {
        // The "gnu" flavour is the ELF driver. The MinGW driver is reached
        // from it through the `-m i386pep` emulation argument that the
        // Windows link line already carries.
        LldFlavor::Elf | LldFlavor::Mingw => "gnu",
        LldFlavor::Darwin => "darwin",
    }
}

pub(super) fn hermetic_link_argv(
    target_name: &str,
    staged: &Path,
    out: &Path,
) -> Result<(LldFlavor, Vec<String>), String> {
    let out_str = out.to_string_lossy().to_string();
    let staged_str = staged.to_string_lossy().to_string();
    // The argv shape is a pure function of the target; asset presence is
    // enforced by the caller (`build_hermetic` stages them first). Keeping
    // this arm free of `assets_for` lets the shape be tested on any host.
    match target_name {
        "x86_64-linux" => Ok((
            LldFlavor::Elf,
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
            LldFlavor::Elf,
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
            Ok((LldFlavor::Mingw, args))
        }
        "x86_64-macos" => Ok((
            LldFlavor::Darwin,
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
            LldFlavor::Darwin,
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
        _ => Err(assets_for(target_name)
            .err()
            .unwrap_or_else(|| format!("cato: no embedded assets for target '{target_name}'"))),
    }
}

#[cfg(feature = "static-llvm")]
pub fn build_hermetic(program: &Program, cli: &Cli, out: &Path) -> Result<PathBuf, String> {
    reject_unsupported(cli)?;
    let target_name = resolve_target_asset_name(&cli.target)?;
    let ir = crate::native::emit_llvm_ir(program, cli).map_err(|e| e.to_string())?;
    if let Some(ref ll_path) = cli.emit_ll {
        fs::write(ll_path, &ir).map_err(|e| format!("cato: cannot write LLVM IR: {}", e))?;
    }
    let obj = objgen::emit_object(&ir, llvm_triple_for_target(&target_name), cli.opt_level)?;
    let staging = unique_temp("cato-hermetic")
        .map_err(|e| format!("cato: cannot create staging dir: {}", e))?;
    let staged = staging.path.clone();
    let prog_o = staged.join("prog.o");
    fs::write(&prog_o, &obj).map_err(|e| format!("cato: cannot write object: {}", e))?;
    for (name, bytes) in assets_for(&target_name)?.files {
        let dst = staged.join(name);
        fs::write(&dst, bytes).map_err(|e| format!("cato: cannot stage asset: {}", e))?;
    }
    let (flavor, argv) = hermetic_link_argv(&target_name, &staged, out)?;
    #[cfg(feature = "in-process-lld")]
    {
        let mut cstrs: Vec<std::ffi::CString> = Vec::with_capacity(argv.len());
        for a in &argv {
            cstrs
                .push(std::ffi::CString::new(a.as_str()).map_err(|_| "cato: invalid argv string")?);
        }
        let refs: Vec<&std::ffi::CStr> = cstrs.iter().map(|c| c.as_c_str()).collect();
        let lld_flavor = match flavor {
            LldFlavor::Elf => cato_lld_sys::Flavor::Elf,
            LldFlavor::Mingw => cato_lld_sys::Flavor::Mingw,
            LldFlavor::Darwin => cato_lld_sys::Flavor::Darwin,
        };
        cato_lld_sys::link(lld_flavor, &refs).map_err(|e| format!("cato: link failed: {}", e))?;
        Ok(out.to_path_buf())
    }
    #[cfg(cato_bundled_lld)]
    {
        let lld = bundled::ensure_lld()?;
        use std::process::Command;
        let mut cmd = Command::new(&lld);
        cmd.arg("-flavor").arg(lld_flavor_option(flavor));
        cmd.args(&argv[1..]);
        let result = cmd.output().map_err(|e| format!("cannot run lld: {}", e))?;
        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            let mut msg = format!(
                "link failed: lld {}",
                crate::native::describe_status(result.status)
            );
            if !stderr.trim_end().is_empty() {
                msg.push_str(":\n");
                msg.push_str(stderr.trim_end());
            }
            return Err(msg);
        }
        Ok(out.to_path_buf())
    }
    #[cfg(all(
        feature = "static-llvm",
        not(feature = "in-process-lld"),
        not(cato_bundled_lld)
    ))]
    {
        Err("cato: hermetic linking unavailable in this build".to_string())
    }
    #[cfg(not(feature = "static-llvm"))]
    {
        let _ = (flavor, argv);
        Err("cato: hermetic linking unavailable in this build".to_string())
    }
}

#[cfg(all(feature = "static-llvm", test))]
mod tests {
    use super::*;

    #[test]
    fn rejects_cc_runtime_asan_rtlib_passthrough() {
        let cli = Cli::default();
        let mut c = cli.clone();
        c.cc = Some("/bin/cc".to_string());
        assert_eq!(
            reject_unsupported(&c).unwrap_err(),
            "cato: --cc is not used in this cato build (it links hermetically; no external C compiler is consulted)"
        );
        let mut c = cli.clone();
        c.runtime = Some("runtime/runtime.c".to_string());
        assert_eq!(
            reject_unsupported(&c).unwrap_err(),
            "cato: --runtime is not supported in this cato build (the runtime is embedded)"
        );
        let mut c = cli.clone();
        c.asan = true;
        assert_eq!(
            reject_unsupported(&c).unwrap_err(),
            "cato: --asan is not supported in this cato build"
        );
        let mut c = cli.clone();
        c.debug = true;
        assert_eq!(
            reject_unsupported(&c).unwrap_err(),
            "cato: -g is not supported in this cato build (in-process codegen emits no debug info)"
        );
        let mut c = cli.clone();
        c.rtlib_given = true;
        assert_eq!(
            reject_unsupported(&c).unwrap_err(),
            "cato: --rtlib is not supported in this cato build (compiler-rt is embedded)"
        );
        let mut c = cli.clone();
        c.passthrough = vec!["-Wl,--no-as-needed".to_string()];
        assert_eq!(
            reject_unsupported(&c).unwrap_err(),
            "cato: passthrough '-Wl,--no-as-needed' is not supported in this cato build"
        );
    }

    #[test]
    fn linux_argv_shape() {
        let t = std::env::temp_dir().join("cato-test");
        let out = t.join("out");
        let (flavor, argv) = hermetic_link_argv("x86_64-linux", &t, &out).unwrap();
        assert!(matches!(flavor, LldFlavor::Elf));
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
        assert!(matches!(flavor, LldFlavor::Elf));
        assert_eq!(argv[0], "ld.lld");
        assert!(!argv.contains(&"-m".to_string()));
    }

    #[test]
    fn windows_argv_shape() {
        let t = std::env::temp_dir().join("cato-test3");
        let out = t.join("out.exe");
        let (flavor, argv) = hermetic_link_argv("x86_64-windows", &t, &out).unwrap();
        assert!(matches!(flavor, LldFlavor::Mingw));
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
        assert!(matches!(flavor, LldFlavor::Darwin));
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
        assert!(matches!(flavor, LldFlavor::Darwin));
        assert!(argv.contains(&"arm64".to_string()));
    }

    #[cfg(cato_bundled_lld)]
    #[test]
    fn bundled_flavor_option_matches_the_driver() {
        assert_eq!(lld_flavor_option(LldFlavor::Elf), "gnu");
        assert_eq!(lld_flavor_option(LldFlavor::Mingw), "gnu");
        assert_eq!(lld_flavor_option(LldFlavor::Darwin), "darwin");
    }

    #[test]
    fn mingw_argv_carries_the_emulation_lld_uses_to_pick_the_driver() {
        let t = std::env::temp_dir().join("cato-test6");
        let out = t.join("out.exe");
        let (_, argv) = hermetic_link_argv("x86_64-windows", &t, &out).unwrap();
        let pos = argv.iter().position(|a| a == "-m").expect("-m present");
        assert_eq!(argv[pos + 1], "i386pep");
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

#[cfg(cato_bundled_lld)]
mod bundled {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn host_target_name() -> &'static str {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            "x86_64-linux"
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            "aarch64-linux"
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            "x86_64-macos"
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            "aarch64-macos"
        }
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            "x86_64-windows"
        }
    }

    pub fn cache_path() -> Result<PathBuf, String> {
        let base = if let Some(h) = std::env::var_os("HOME") {
            PathBuf::from(h)
        } else if let Some(up) = std::env::var_os("USERPROFILE") {
            PathBuf::from(up)
        } else {
            return Err("cato: cannot determine home directory for bundled lld cache".to_string());
        };
        let name = if cfg!(target_os = "windows") {
            "lld.exe"
        } else {
            "lld"
        };
        Ok(base
            .join(".cache")
            .join("cato")
            .join(format!("lld-{}", env!("CARGO_PKG_VERSION")))
            .join(host_target_name())
            .join(name))
    }

    fn lock_is_stale(lock: &Path) -> bool {
        lock.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age > Duration::from_secs(60))
            .unwrap_or(false)
    }

    pub fn extract_to(dest: &Path, bytes: &[u8], timeout: Duration) -> Result<(), String> {
        let parent = dest
            .parent()
            .ok_or_else(|| "cato: invalid cache path".to_string())?;
        fs::create_dir_all(parent).map_err(|e| format!("cato: cannot create cache dir: {}", e))?;
        let lock = dest.with_extension("lock");
        let start = SystemTime::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(mut f) => {
                    let _ = f.write_all(b"1");
                    break;
                }
                Err(_) => {
                    if lock_is_stale(&lock) {
                        let _ = fs::remove_file(&lock);
                        continue;
                    }
                    if start.elapsed().map(|e| e > timeout).unwrap_or(true) {
                        return Err(format!(
                            "cato: timed out waiting for bundled lld extraction lock {}",
                            lock.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        let result = (|| -> Result<(), String> {
            if let Ok(meta) = dest.metadata() {
                if meta.len() as usize == bytes.len() {
                    return Ok(());
                }
                return Err(format!(
                    "cato: bundled lld is missing or corrupt (expected {} bytes, found {}) — reinstall cato",
                    bytes.len(),
                    meta.len()
                ));
            }
            let tmp = dest.with_extension("tmp");
            fs::write(&tmp, bytes)
                .map_err(|e| format!("cato: failed to extract bundled lld: {}", e))?;
            fs::rename(&tmp, dest)
                .map_err(|e| format!("cato: failed to extract bundled lld: {}", e))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = dest
                    .metadata()
                    .map_err(|e| format!("cato: failed to extract bundled lld: {}", e))?
                    .permissions();
                perm.set_mode(0o755);
                let _ = fs::set_permissions(dest, perm);
            }
            Ok(())
        })();
        let _ = fs::remove_file(&lock);
        result
    }

    pub fn ensure_lld() -> Result<PathBuf, String> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let bytes: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cato-assets/assets/lld/x86_64-linux/lld"
        ));
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let bytes: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cato-assets/assets/lld/aarch64-linux/lld"
        ));
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        let bytes: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cato-assets/assets/lld/x86_64-macos/lld"
        ));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let bytes: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cato-assets/assets/lld/aarch64-macos/lld"
        ));
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        let bytes: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cato-assets/assets/lld/x86_64-windows/lld.exe"
        ));
        let dest = cache_path()?;
        extract_to(&dest, bytes, Duration::from_secs(10))?;
        Ok(dest)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tmp_path(name: &str) -> PathBuf {
            std::env::temp_dir().join(format!("cato-bundled-test-{}-{}", std::process::id(), name))
        }

        #[test]
        fn extraction_is_exactly_once_under_races() {
            let dest = tmp_path("race");
            let _ = fs::remove_file(&dest);
            let a = vec![7u8; 4096];
            let b = vec![9u8; 4096];
            let da = dest.clone();
            let db = dest.clone();
            let ta = std::thread::spawn(move || extract_to(&da, &a, Duration::from_secs(10)));
            let tb = std::thread::spawn(move || extract_to(&db, &b, Duration::from_secs(10)));
            assert!(ta.join().unwrap().is_ok());
            assert!(tb.join().unwrap().is_ok());
            let got = fs::read(&dest).expect("read dest");
            assert_eq!(got.len(), 4096);
            assert!(
                got.iter().all(|&x| x == 7) || got.iter().all(|&x| x == 9),
                "dest must match one writer whole"
            );
            let _ = fs::remove_file(&dest);
        }

        #[test]
        fn size_mismatch_reports_reinstall() {
            let dest = tmp_path("mismatch");
            let _ = fs::remove_file(&dest);
            let _ = fs::remove_file(dest.with_extension("lock"));
            fs::write(&dest, b"abc").expect("seed");
            let e = extract_to(&dest, b"hello", Duration::from_secs(2)).expect_err("must err");
            assert_eq!(
                e,
                "cato: bundled lld is missing or corrupt (expected 5 bytes, found 3) — reinstall cato"
            );
            let _ = fs::remove_file(&dest);
        }

        #[test]
        fn lock_timeout_is_an_error() {
            let dest = tmp_path("lock");
            let _ = fs::remove_file(&dest);
            let lock = dest.with_extension("lock");
            let _ = fs::remove_file(&lock);
            fs::write(&lock, b"1").expect("hold lock");
            let e = extract_to(&dest, b"hello", Duration::from_millis(1)).expect_err("must err");
            assert!(e.contains("timed out"), "got: {e}");
            assert!(e.contains(&lock.to_string_lossy().to_string()), "got: {e}");
            let _ = fs::remove_file(&lock);
            let _ = fs::remove_file(&dest);
        }
    }
}
