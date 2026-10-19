#![cfg(any(feature = "in-process-lld", feature = "static-llvm"))]

use std::env;
use std::fs;
use std::process::Command;

fn cato() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cato"))
}

fn src() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/slice.cato")
}

#[cfg(feature = "in-process-lld")]
#[test]
fn hermetic_rejects_cc_flag() {
    let tmp = env::temp_dir().join(format!("cato-cc-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let out = tmp.join("slice");
    let mut cmd = cato();
    cmd.arg("compile")
        .arg(src())
        .arg("-o")
        .arg(&out)
        .arg("--cc")
        .arg("/bin/false");
    let out = cmd.output().expect("run");
    assert!(!out.status.success());
}

#[cfg(feature = "in-process-lld")]
#[test]
fn hermetic_rejects_runtime_flag() {
    let tmp = env::temp_dir().join(format!("cato-rt-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let out = tmp.join("slice");
    let mut cmd = cato();
    cmd.arg("compile")
        .arg(src())
        .arg("-o")
        .arg(&out)
        .arg("--runtime")
        .arg("runtime/runtime.c");
    let out = cmd.output().expect("run");
    assert!(!out.status.success());
}

#[cfg(feature = "in-process-lld")]
#[test]
fn hermetic_host_compile_runs_without_cc() {
    let tmp = env::temp_dir().join(format!("cato-host-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let out = tmp.join("slice");
    let status = cato()
        .arg("compile")
        .arg(src())
        .arg("-o")
        .arg(&out)
        .env("PATH", "")
        .env_remove("CC")
        .env_remove("CATO_CC")
        .env_remove("CATO_RUNTIME")
        .status()
        .expect("compile");
    assert!(status.success());
    let run = Command::new(&out).output().expect("run");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2.5\n20\n");
    #[cfg(target_os = "linux")]
    {
        let bytes = fs::read(&out).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF");
    }
    #[cfg(target_os = "macos")]
    {
        let bytes = fs::read(&out).unwrap();
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!(magic == 0xfeedfacf || magic == 0xcffaedfe, "macho magic");
    }
}

#[cfg(feature = "in-process-lld")]
#[test]
fn hermetic_cross_linux_is_elf() {
    let tmp = env::temp_dir().join(format!("cato-elf-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    for target in ["x86_64-linux", "aarch64-linux"] {
        let out = tmp.join(format!("slice-{}", target));
        let status = cato()
            .arg("compile")
            .arg(src())
            .arg("-o")
            .arg(&out)
            .arg("--target")
            .arg(target)
            .status()
            .expect("compile");
        assert!(status.success(), "compile for {}", target);
        let bytes = fs::read(&out).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF", "{}", target);
    }
}

#[cfg(feature = "in-process-lld")]
#[test]
fn hermetic_cross_windows_is_pe() {
    let tmp = env::temp_dir().join(format!("cato-pe-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let out = tmp.join("slice.exe");
    let status = cato()
        .arg("compile")
        .arg(src())
        .arg("-o")
        .arg(&out)
        .arg("--target")
        .arg("x86_64-windows")
        .status()
        .expect("compile");
    assert!(status.success());
    let bytes = fs::read(&out).unwrap();
    assert_eq!(&bytes[..2], b"MZ");
}

#[cfg(feature = "in-process-lld")]
#[test]
fn hermetic_cross_macos_is_macho() {
    let tmp = env::temp_dir().join(format!("cato-macho-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    for target in ["x86_64-macos", "aarch64-macos"] {
        let out = tmp.join(format!("slice-{}", target));
        let status = cato()
            .arg("compile")
            .arg(src())
            .arg("-o")
            .arg(&out)
            .arg("--target")
            .arg(target)
            .status()
            .expect("compile");
        assert!(status.success(), "compile for {}", target);
        let bytes = fs::read(&out).unwrap();
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!(magic == 0xfeedfacf || magic == 0xcffaedfe, "{}", target);
    }
}

#[cfg(feature = "in-process-lld")]
#[cfg(target_os = "macos")]
#[test]
fn hermetic_macos_output_links_only_libsystem() {
    let tmp = env::temp_dir().join(format!("cato-sys-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let out = tmp.join("slice");
    let status = cato()
        .arg("compile")
        .arg(src())
        .arg("-o")
        .arg(&out)
        .status()
        .expect("compile");
    assert!(status.success());
    let deps = Command::new("otool")
        .arg("-L")
        .arg(&out)
        .output()
        .expect("otool");
    let s = String::from_utf8_lossy(&deps.stdout);
    let lines: Vec<&str> = s
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "exactly one dependency: {}", s);
    assert!(
        lines[0].trim().contains("/usr/lib/libSystem.B.dylib"),
        "{}",
        s
    );
}


#[cfg(all(feature = "static-llvm", not(feature = "in-process-lld")))]
#[test]
fn bundled_link_produces_running_host_binary() {
    let tmp = env::temp_dir().join(format!("cato-bundled-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let out = tmp.join("slice");
    let c = cato()
        .arg("compile")
        .arg(src())
        .arg("-o")
        .arg(&out)
        .env("PATH", "")
        .env_remove("CC")
        .env_remove("CATO_CC")
        .env_remove("CATO_RUNTIME")
        .status()
        .expect("compile");
    assert!(c.success());
    let run = std::process::Command::new(&out).output().expect("run");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2.5\n20\n");
    let cache = env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache")
        .join("cato");
    assert!(cache.exists(), "bundled lld cache dir should exist: {}", cache.display());
}
