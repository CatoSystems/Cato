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
