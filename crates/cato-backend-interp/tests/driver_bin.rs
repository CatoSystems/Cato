//! The `cato-interp-run` driver bin: the interpreter's process-boundary
//! harness (the interpreter calls std::process::exit, so programs with
//! `(exit n)` — like the kernel CLI — must run behind a process).

use std::process::Command;

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn driver() -> Command {
    Command::new(root().join("target/debug/cato-interp-run"))
}

#[test]
fn driver_bin_runs_slice_and_prints_output() {
    let out = driver()
        .arg(root().join("examples/slice.cato"))
        .output()
        .expect("spawn cato-interp-run");
    assert!(
        out.status.success(),
        "got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2.5\n20\n");
}

#[test]
fn driver_bin_propagates_exit_code() {
    let src = root().join("target/driver_bin_exit.cato");
    std::fs::write(&src, "(defn main [] (exit 7))").expect("write src");
    let out = driver().arg(&src).output().expect("spawn cato-interp-run");
    assert_eq!(
        out.status.code(),
        Some(7),
        "got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn driver_bin_reports_compile_errors_with_exit_1() {
    let src = root().join("target/driver_bin_bad.cato");
    std::fs::write(&src, "(defn main [] (print (+ 1 \"nope\")))").expect("write src");
    let out = driver().arg(&src).output().expect("spawn cato-interp-run");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cato: error:"), "got: {}", stderr);
}
