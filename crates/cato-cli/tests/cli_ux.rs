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
        assert!(
            text.contains("EXIT STATUS"),
            "{} must document exit codes",
            help
        );
    }
}

#[test]
fn no_args_shows_friendly_banner() {
    let out = cato().output().expect("run cato");
    assert_eq!(out.status.code(), Some(2), "no args is a usage error");
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("quick start"),
        "banner must guide: {}",
        stderr
    );
    assert!(stderr.contains("--help"), "banner must point at help");
}

#[test]
fn unknown_flag_suggests_closest_match() {
    let src = demo_source();
    let out = cato()
        .arg("compile")
        .arg(&src)
        .arg("--emit-irr")
        .output()
        .expect("run cato");
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
    assert!(
        stderr.contains("cato compile"),
        "must suggest compile: {}",
        stderr
    );
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

// The removed run command points at compile + repl.
#[test]
fn run_command_is_retired() {
    let src = demo_source();
    let out = cato().arg("run").arg(&src).output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cato repl"), "got: {}", stderr);
    assert!(stderr.contains("removed"), "got: {}", stderr);
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
        assert!(
            text.contains(needle),
            "help must document {:?}:\n{}",
            needle,
            text
        );
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
    let out = cato()
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--emit-ll", "/tmp/should_not_exist_x.ll"])
        .output()
        .expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--emit-ir"), "got: {}", stderr);
    assert!(stderr.contains("--emit-ll"), "got: {}", stderr);
}

#[test]
fn two_positional_files_are_rejected() {
    let src = demo_source();
    let out = cato()
        .arg("compile")
        .arg(&src)
        .arg(&src)
        .output()
        .expect("run cato");
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
    let out = cato()
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run cato");
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "stdout must stay clean");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("compiled"), "got: {}", stderr);
    assert!(
        stderr.contains("ms"),
        "summary should include timing: {}",
        stderr
    );
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

// Short typos get proportionate suggestions: `-O` is not two edits from
// `--cc` in any useful sense.
#[test]
fn short_flag_typo_suggests_opt_level() {
    let src = demo_source();
    let out = cato()
        .arg("compile")
        .arg(&src)
        .arg("-O")
        .output()
        .expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean '-O2'"), "got: {}", stderr);
    assert!(!stderr.contains("--cc"), "got: {}", stderr);
}

// A lone mistyped command is named, not shown as a '<file>' placeholder.
#[test]
fn lone_mistyped_command_suggests_closest_match() {
    let out = cato().arg("compil").output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean 'compile'"), "got: {}", stderr);
    assert!(!stderr.contains("<file>"), "got: {}", stderr);
}

// Every subcommand answers --help on stdout; repl/lsp reject stray args.
#[test]
fn subcommands_answer_help_and_reject_stray_args() {
    for sub in ["repl", "lsp", "format"] {
        let out = cato().arg(sub).arg("--help").output().expect("run cato");
        assert!(out.status.success(), "{sub} --help must succeed");
        assert!(!out.stdout.is_empty(), "{sub} --help must print to stdout");
    }
    for sub in ["repl", "lsp"] {
        let out = cato().arg(sub).arg("extra").output().expect("run cato");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{sub} with an argument is a usage error"
        );
    }
}

// Toolchain failures name the compiler and print the status once, with a
// single `cato: error:` lead.
#[cfg(not(feature = "static-llvm"))]
#[test]
fn toolchain_failures_name_the_compiler() {
    let src = demo_source();
    let bin = std::env::temp_dir().join(format!("cato_ux_cc_{}", std::process::id()));
    let missing = cato()
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .args(["--cc", "/definitely/not/a/cc"])
        .output()
        .expect("run cato");
    assert_eq!(missing.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("'/definitely/not/a/cc'"), "got: {}", stderr);
    assert!(stderr.contains("--cc"), "must point at the fix: {}", stderr);

    let failing = cato()
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .args(["--cc", "false"])
        .output()
        .expect("run cato");
    assert_eq!(failing.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&failing.stderr);
    assert!(
        stderr.contains("false exited with status 1"),
        "got: {}",
        stderr
    );
    assert!(!stderr.contains("exit exit"), "got: {}", stderr);
    assert_eq!(stderr.matches("cato:").count(), 1, "one lead: {}", stderr);
}

// A lowering error points at the failing sub-form, not the enclosing defn.
#[test]
fn diagnostics_point_at_the_failing_subform() {
    let bad = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/ux_errloc.cato");
    std::fs::write(
        &bad,
        "(defn main [] :- i64\n  (let [a 1]\n    (+ a \"x\")))\n",
    )
    .expect("write bad");
    let out = cato().arg("compile").arg(&bad).output().expect("run cato");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ux_errloc.cato:3:5"), "got: {}", stderr);
    assert!(
        stderr.contains("(+ a \"x\")"),
        "snippet shows the form: {}",
        stderr
    );
}

// Ownership errors point at the offending occurrence, not the defn.
#[test]
fn ownership_errors_point_at_the_occurrence() {
    let bad = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/ux_errloc_move.cato");
    std::fs::write(
        &bad,
        // `take` keeps (stores) its argument: a callee that only read it
        // would just lend it, and the program would be valid.
        "(deftype Bx (Bx [v :- (Vector i64)]))\n\
         (defn take [v :- (Vector i64)] :- Bx (Bx v))\n\
         (defn main [] :- i64\n  (let [xs (vec-i64 1 2)]\n    (do (take xs) (count xs))))\n",
    )
    .expect("write bad");
    let out = cato().arg("compile").arg(&bad).output().expect("run cato");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("use of moved value xs"), "got: {}", stderr);
    assert!(
        stderr.contains("ux_errloc_move.cato:5:26"),
        "got: {}",
        stderr
    );
}
