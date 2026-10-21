//! REPL interaction tests (piped stdin, so no colors and no line editing —
//! the fallback reader shares all evaluation logic with the interactive
//! raw-mode editor).

use std::process::{Command, Stdio};

fn repl(input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cato repl");
    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write input");
    let out = child.wait_with_output().expect("repl output");
    assert!(
        out.status.success(),
        "repl failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn repl_has_clojure_style_prompt_and_results() {
    let out = repl("(defn sq [x :- i64] (* x x))\n(sq 7)\n:quit\n");
    assert!(out.contains("user=> "), "prompt must be ns + =>: {}", out);
    assert!(!out.contains("cato=>"), "old cato prefix must be gone");
    // Definitions echo Clojure-style vars; results on their own line.
    assert!(out.contains("#'user/sq"), "{}", out);
    assert!(out.contains("\n49\n"), "{}", out);
    // Piped output carries no ANSI codes.
    assert!(!out.contains('\x1b'), "no colors when piped: {}", out);
}

#[test]
fn repl_reads_multi_line_until_parens_balance() {
    let out = repl("(defn area [w :- i64 h :- i64]\n  (* w h))\n(area 3 4)\n:quit\n");
    assert!(
        out.contains("#_=>"),
        "continuation prompt expected: {}",
        out
    );
    assert!(out.contains("#'user/area"), "{}", out);
    assert!(out.contains("\n12\n"), "{}", out);
}

#[test]
fn repl_survives_errors_and_keeps_session() {
    let out = repl("(sq 7)\n(defn sq [x :- i64] (* x x))\n(sq 7)\n:quit\n");
    // The first call fails, but the session continues working after.
    assert!(out.contains("error:"), "{}", out);
    assert!(out.contains("\n49\n"), "{}", out);
}

#[test]
fn repl_failed_definition_is_rolled_back() {
    let out = repl("(defn bad [x :- i64] (+ x \"nope\"))\n(+ 1 2)\n:quit\n");
    assert!(out.contains("error:"), "def error must surface: {}", out);
    // The session still evaluates fine afterwards.
    assert!(out.contains("\n3\n"), "{}", out);
    assert!(
        !out.contains("#'user/bad"),
        "failed def must not confirm: {}",
        out
    );
}

#[test]
fn repl_rejects_unbalanced_closing_paren() {
    let out = repl("(+ 1 2))\n:quit\n");
    assert!(out.contains("unbalanced"), "{}", out);
    assert!(
        !out.contains("\n3\n"),
        "the form must not evaluate: {}",
        out
    );
}

#[test]
fn repl_ns_form_switches_prompt() {
    let out = repl("(ns geometry)\n(+ 1 2)\n:quit\n");
    assert!(out.contains("geometry=> "), "{}", out);
}

#[test]
fn repl_eof_evaluates_pending_form_and_exits() {
    let out = repl("(+ 40 2)\n");
    assert!(out.contains("\n42\n"), "{}", out);
}

#[test]
fn repl_help_lists_commands() {
    let out = repl(":help\n:quit\n");
    assert!(out.contains(":quit"), "{}", out);
}

// Interpreting malformed input that trips internal invariant panics must
// produce an error, never a dead REPL: the session continues afterwards.
#[test]
fn repl_survives_runtime_panics() {
    let out = repl("(count 5)\n(if 1 2 3)\n(+ 1 2)\n:quit\n");
    assert!(!out.contains("panicked"), "raw panic leaked: {}", out);
    assert!(
        out.contains("error:"),
        "count on non-vector must error: {}",
        out
    );
    assert!(out.contains("error:"), "if on non-bool must error: {}", out);
    assert!(out.contains("\n3\n"), "session must keep working: {}", out);
}

// Every documented exit word terminates the REPL cleanly.
#[test]
fn repl_exit_words() {
    for word in [":quit", ":q", ":exit", "exit", "quit"] {
        let out = repl(&format!("(+ 1 2)\n{word}\n"));
        assert!(out.contains("\n3\n"), "{word}: {}", out);
        assert!(
            !out.contains("error:") || out.contains("quit"),
            "{word} must not evaluate as a form: {}",
            out
        );
    }
}

// Regression: raw mode disables the terminal's LF->CRLF translation, so a
// bare newline on Enter left the cursor mid-column and every result line
// printed indented. Through a pty, the result must start at column 0.
// Skipped where the BSD `script` pty helper is unavailable.
#[test]
fn repl_results_start_at_column_zero_on_a_terminal() {
    let script = std::path::Path::new("/usr/bin/script");
    if !script.exists() {
        return;
    }
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = Command::new(script)
        .args(["-q", "/dev/null", env!("CARGO_BIN_EXE_cato"), "repl"])
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn script");
    let stdin = child.stdin.as_mut().expect("stdin");
    let _ = stdin.write_all(b"(+ 1 2)\r");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let _ = stdin.write_all(b":quit\r");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let out = child.wait_with_output().expect("repl output");
    let raw = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        raw.contains("\r\n\x1b[1m3\x1b[0m"),
        "result must begin at column 0 after CRLF; got: {:?}",
        raw
    );
}
