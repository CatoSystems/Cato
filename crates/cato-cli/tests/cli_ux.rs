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
    let out = cato().arg("compile").arg(&src).arg("--emit-irr").output().expect("run cato");
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
    let out = cato().arg("comple").arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean 'compile'"), "got: {}", stderr);
}

// A bare input file must be routed through a subcommand, not guessed.
#[test]
fn bare_file_requires_subcommand() {
    let src = demo_source();
    let out = cato().arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("subcommand required"), "got: {}", stderr);
    assert!(stderr.contains("cato compile"), "must suggest compile: {}", stderr);
}

// The retired build command gets an explicit migration message.
#[test]
fn build_command_is_retired() {
    let src = demo_source();
    let out = cato().arg("build").arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("renamed to 'compile'"), "got: {}", stderr);
}

#[test]
fn compile_without_file_is_usage_error() {
    let out = cato().arg("compile").output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("requires an input file"), "got: {}", stderr);
}

// Help documents the contract surface: compile forms, every new flag,
// `--` passthrough, exit codes — and never the removed `run`.
#[test]
fn help_documents_contract_surface() {
    let out = cato().arg("help").output().expect("run cato");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in ["cato compile", "--rtlib", "--runtime", "EXIT STATUS"] {
        assert!(text.contains(needle), "help must document {:?}:\n{}", needle, text);
    }
    assert!(
        text.contains("forwarded verbatim"),
        "help must document -- passthrough:\n{}",
        text
    );
    assert!(
        !text.contains("cato run"),
        "help must not offer the removed run command:\n{}",
        text
    );
}

#[test]
fn emit_ir_and_emit_ll_together_rejected() {
    let src = demo_source();
    let out = cato().arg("compile").arg(&src)
        .args(["--emit-ir", "--emit-ll", "/tmp/should_not_exist_x.ll"])
        .output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--emit-ir"), "got: {}", stderr);
    assert!(stderr.contains("--emit-ll"), "got: {}", stderr);
}

#[test]
fn two_positional_files_are_rejected() {
    let src = demo_source();
    let out = cato().arg("compile").arg(&src).arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("only one input file"),
        "got: {}",
        stderr
    );
}

// Compile success keeps stdout clean but gives the user feedback on stderr.
#[test]
fn check_mode_summarizes_on_stderr() {
    let src = demo_source();
    let bin = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/ux_bin_demo");
    let out = cato().arg("compile").arg(&src).arg("-o").arg(&bin).output().expect("run cato");
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "stdout must stay clean");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("compiled"), "got: {}", stderr);
    assert!(stderr.contains("ms"), "summary should include timing: {}", stderr);
    let _ = std::fs::remove_file(&bin);
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

    let compile_err = cato().arg("compile").arg(&bad).output().expect("run cato");
    assert_eq!(compile_err.status.code(), Some(1), "compile error -> 1");

    let usage_err = cato().arg("--no-such-flag").output().expect("run cato");
    assert_eq!(usage_err.status.code(), Some(2), "usage error -> 2");
}
