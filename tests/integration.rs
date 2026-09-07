// tests/integration.rs
use std::process::Command;

#[test]
fn cli_hello_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("--help")
        .output()
        .expect("run binary");
    assert!(out.status.success());
}

#[test]
fn cli_rejects_dangling_o_flag() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("examples/slice.cato");
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src)
        .args(["-o"])
        .output()
        .expect("run binary");
    assert!(!out.status.success(), "expected -o without filename to fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-o requires a filename"), "got: {}", stderr);
}

#[test]
fn cli_rejects_unknown_flag() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("examples/slice.cato");
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src)
        .arg("--no-such-flag")
        .output()
        .expect("run binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"), "got: {}", stderr);
}

// End-to-end: parse/build/fuse/emit to IR, compile with clang, run, assert output.
#[test]
fn compiles_slice_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("examples/slice.cato");
    let out_ll = manifest.join("target/slice.ll");
    let rt = manifest.join("runtime/runtime.c");
    let exe = manifest.join("target/slice_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src).arg("-o").arg(&out_ll)
        .status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "2.5\n20\n");
}
