//! `cato format` CLI contract (spec §1/§3): in-place rewrite, `--check`
//! CI mode, `--width`, stdin, per-file errors, help listing.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cato")
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cato-fmt-cli-{}-{}", std::process::id(), label));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

const MESSY: &str = "(defn   f []  (+ 1 2))\n";
const NEAT: &str = "(defn f [] (+ 1 2))\n";

#[test]
fn format_rewrites_in_place_and_is_quiet() {
    let dir = scratch("rewrite");
    let file = dir.join("a.cato");
    fs::write(&file, MESSY).unwrap();
    let out = Command::new(bin())
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty in rewrite mode"
    );
    assert!(out.stderr.is_empty(), "stderr should be empty on success");
    assert_eq!(fs::read_to_string(&file).unwrap(), NEAT);
    // A second run changes nothing (idempotent through the CLI).
    let again = Command::new(bin())
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(again.status.success());
    assert_eq!(fs::read_to_string(&file).unwrap(), NEAT);
}

#[test]
fn check_mode_reports_and_writes_nothing() {
    let dir = scratch("check");
    let file = dir.join("a.cato");
    fs::write(&file, MESSY).unwrap();
    let before = fs::read(&file).unwrap();
    let out = Command::new(bin())
        .args(["format", "--check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(&file).unwrap(), before, "--check must not write");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(file.to_str().unwrap()),
        "stdout should list the path, got: {stdout}"
    );
}

#[test]
fn check_mode_clean_file_exits_zero() {
    let dir = scratch("check-clean");
    let file = dir.join("a.cato");
    fs::write(&file, NEAT).unwrap();
    let out = Command::new(bin())
        .args(["format", "--check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "clean check should print nothing");
}

#[test]
fn width_flag_controls_breaking() {
    let src = "(defn f [a :- i64 b :- i64 c :- i64 d :- i64 e :- i64] :- i64 \
                (+ a (+ b (+ c (+ d e)))))\n";
    let dir = scratch("width");
    let file = dir.join("a.cato");
    fs::write(&file, src).unwrap();
    let out = Command::new(bin())
        .args(["format", "--width", "40", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let formatted = fs::read_to_string(&file).unwrap();
    let expected = cato_fmt::format_source(src, 40).unwrap();
    assert_eq!(formatted, expected);
    assert!(
        formatted.lines().count() > 1,
        "width 40 should break the form:\n{formatted}"
    );
}

#[test]
fn width_out_of_range_exits_two() {
    let dir = scratch("width-range");
    let file = dir.join("a.cato");
    fs::write(&file, NEAT).unwrap();
    let out = Command::new(bin())
        .args(["format", "--width", "5", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(bin())
        .args(["format", "--width", "5000", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn unknown_flag_exits_two_with_suggestion() {
    let dir = scratch("flag");
    let file = dir.join("a.cato");
    fs::write(&file, NEAT).unwrap();
    let out = Command::new(bin())
        .args(["format", "--chek", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--check"),
        "suggestion expected, got: {stderr}"
    );
}

#[test]
fn stdin_dash_formats_to_stdout() {
    let mut child = Command::new(bin())
        .args(["format", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(MESSY.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), NEAT);
}

#[test]
fn missing_file_exits_one() {
    let out = Command::new(bin())
        .args(["format", "nope.cato"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cato: "), "got: {stderr}");
    assert!(stderr.contains("nope.cato"), "got: {stderr}");
}

#[test]
fn help_lists_format() {
    let out = Command::new(bin()).arg("help").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("format"),
        "help should list format:\n{stdout}"
    );
}

#[test]
fn malformed_file_exits_one_and_keeps_other_files() {
    let dir = scratch("partial");
    let bad = dir.join("bad.cato");
    let good = dir.join("good.cato");
    fs::write(&bad, "(defn f [\n").unwrap();
    fs::write(&good, MESSY).unwrap();
    let out = Command::new(bin())
        .args(["format", bad.to_str().unwrap(), good.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cato: "), "got: {stderr}");
    assert!(stderr.contains("bad.cato"), "got: {stderr}");
    assert_eq!(
        fs::read_to_string(&good).unwrap(),
        NEAT,
        "later files still formatted"
    );
    assert_eq!(
        fs::read_to_string(&bad).unwrap(),
        "(defn f [\n",
        "bad file untouched"
    );
}

#[test]
fn check_mode_on_stdin_reports_change_without_writing() {
    let mut child = Command::new(bin())
        .args(["format", "--check", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(MESSY.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "check on would-change stdin must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "-",
        "stdout must be only the operand, got: {stdout}"
    );
    assert!(
        !stdout.contains("defn"),
        "check mode must not print formatted source, got: {stdout}"
    );
}

#[test]
fn check_mode_on_clean_stdin_exits_zero() {
    let mut child = Command::new(bin())
        .args(["format", "--check", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(NEAT.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "clean stdin check should print nothing"
    );
}

#[test]
fn rewrite_replaces_file_atomically() {
    let dir = scratch("atomic");
    let file = dir.join("a.cato");
    fs::write(&file, MESSY).unwrap();
    // Inode identity is the portable signal on Unix; Windows has no
    // stable equivalent, so that part of the check is Unix-only.
    #[cfg(unix)]
    let ino_before = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&file).unwrap().ino()
    };
    let out = Command::new(bin())
        .args(["format", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Atomic replacement (rename over the original) changes the inode;
    // truncate-in-place would keep it. No temp debris may remain.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ino_after = fs::metadata(&file).unwrap().ino();
        assert_ne!(
            ino_before, ino_after,
            "rewrite must replace via rename, not truncate-in-place"
        );
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), NEAT);
    let entries: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "no temp files may be left behind");
}
