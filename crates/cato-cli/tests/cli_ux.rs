//! CLI user-experience tests: help discoverability, suggestions, exit
//! codes, and the check-mode summary. All run piped (no TTY), so output is
//! asserted without color codes.

use std::process::Command;

fn cato() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cato"))
}

fn demo_source() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/ux_demo.cato");
    std::fs::write(
        &path,
        "(defn sq [x :- i64] (* x x))\n(defn main [] (print (sq 7)))\n",
    )
    .expect("write demo");
    path
}

#[test]
fn help_is_on_stdout_and_exits_zero() {
    for help in ["--help", "-h", "help"] {
        let out = cato().arg(help).output().expect("run cato");
        assert!(out.status.success(), "{} must succeed", help);
        assert!(!out.stdout.is_empty(), "{} must print to stdout", help);
        assert!(out.stderr.is_empty(), "{} must not print to stderr", help);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("USAGE"), "{} must show usage", help);
        assert!(text.contains("EXIT STATUS"), "{} must document exit codes", help);
    }
}

#[test]
fn no_args_shows_friendly_banner() {
    let out = cato().output().expect("run cato");
    assert_eq!(out.status.code(), Some(2), "no args is a usage error");
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("quick start"), "banner must guide: {}", stderr);
    assert!(stderr.contains("--help"), "banner must point at help");
}

#[test]
fn unknown_flag_suggests_closest_match() {
    let src = demo_source();
    let out = cato().arg(&src).arg("--emit-irr").output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("did you mean '--emit-ir'"),
        "got: {}",
        stderr
    );
}

#[test]
fn unknown_command_suggests_closest_match() {
    let src = demo_source();
    let out = cato().arg("bild").arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean 'build'"), "got: {}", stderr);
}

#[test]
fn two_positional_files_are_rejected() {
    let src = demo_source();
    let out = cato().arg(&src).arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("only one input file"),
        "got: {}",
        stderr
    );
}

// Check mode keeps stdout clean but gives the user feedback on stderr.
#[test]
fn check_mode_summarizes_on_stderr() {
    let src = demo_source();
    let out = cato().arg(&src).output().expect("run cato");
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "stdout must stay clean");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("compiled"), "got: {}", stderr);
    assert!(stderr.contains("ms"), "summary should include timing: {}", stderr);
}

// Exit codes: compile errors are 1, usage errors stay 2.
#[test]
fn exit_code_tiers() {
    let bad = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/ux_bad.cato");
    std::fs::write(&bad, "(defn main [] (print (+ 1 \"nope\")))").expect("write bad");

    let compile_err = cato().arg(&bad).output().expect("run cato");
    assert_eq!(compile_err.status.code(), Some(1), "compile error -> 1");

    let usage_err = cato().arg("--no-such-flag").output().expect("run cato");
    assert_eq!(usage_err.status.code(), Some(2), "usage error -> 2");
}
