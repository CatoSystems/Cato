//! Golden contract for `format_source`: byte-exact fixtures, idempotency
//! on fixture outputs and the real corpus, and loud failure on malformed
//! input (spec §3).

use cato_fmt::format_source;
use std::fs;
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// All `NAME.in.cato` / `NAME.out.cato` pairs, sorted by name.
fn pairs() -> Vec<(String, PathBuf, PathBuf)> {
    let dir = golden_dir();
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("golden dir").flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".in.cato") {
            out.push((
                stem.to_string(),
                path.clone(),
                dir.join(format!("{stem}.out.cato")),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn golden_files_format_exactly() {
    let pairs = pairs();
    assert!(
        pairs.len() >= 8,
        "expected at least 8 golden pairs, found {}",
        pairs.len()
    );
    for (name, input, expected) in pairs {
        let src = fs::read_to_string(&input).unwrap();
        let want = fs::read_to_string(&expected).unwrap();
        let got = format_source(&src, 100).unwrap_or_else(|e| panic!("{name}: format failed: {e}"));
        assert_eq!(got, want, "golden mismatch: {name}");
    }
}

#[test]
fn idempotent_on_golden_outputs() {
    for (name, _, expected) in pairs() {
        let want = fs::read_to_string(&expected).unwrap();
        let once = format_source(&want, 100)
            .unwrap_or_else(|e| panic!("{name}: reformat of output failed: {e}"));
        assert_eq!(once, want, "not idempotent on first reformat: {name}");
        let twice = format_source(&once, 100).unwrap();
        assert_eq!(twice, once, "not idempotent on second reformat: {name}");
    }
}

/// Plan §Task 4 corpus: `examples/*.cato` plus every `*.cato` under a
/// `fixtures/` directory anywhere in `selfhost/` (skipping `err_*`,
/// the intentionally-malformed reader fixtures).
fn corpus_files() -> Vec<PathBuf> {
    let root = corpus_root();
    let mut files = Vec::new();
    for entry in fs::read_dir(root.join("examples"))
        .expect("examples dir")
        .flatten()
    {
        let path = entry.path();
        if path.extension().map(|x| x == "cato").unwrap_or(false) {
            files.push(path);
        }
    }
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("selfhost dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().map(|x| x == "cato").unwrap_or(false)
                && path.components().any(|c| c.as_os_str() == "fixtures")
            {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !name.starts_with("err_") {
                    out.push(path);
                }
            }
        }
    }
    walk(&root.join("selfhost"), &mut files);
    files.sort();
    files
}

#[test]
fn idempotent_on_real_corpus() {
    let files = corpus_files();
    assert!(
        files.len() >= 60,
        "corpus must sweep examples + all selfhost fixtures recursively, found only {}",
        files.len()
    );
    assert!(
        files
            .iter()
            .any(|p| p.components().any(|c| c.as_os_str() == "hir")),
        "corpus must include selfhost/cato/hir/fixtures"
    );
    for path in files {
        let src = fs::read_to_string(&path).unwrap();
        let once = format_source(&src, 100).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let twice = format_source(&once, 100)
            .unwrap_or_else(|e| panic!("{} (second pass): {e}", path.display()));
        assert_eq!(once, twice, "not idempotent: {}", path.display());
    }
}

#[test]
fn malformed_input_is_error() {
    let e = format_source("(defn f [", 100).unwrap_err();
    assert!(
        e.msg.contains("unclosed") || e.msg.contains("unbalanced"),
        "unexpected message: {}",
        e.msg
    );
}

#[test]
fn map_odd_input_is_error() {
    let e = format_source("{1 2 3}", 100).unwrap_err();
    assert!(
        e.msg.contains("map literal must have key/value pairs"),
        "unexpected message: {}",
        e.msg
    );
}

#[test]
fn unterminated_string_is_error() {
    let e = format_source("(defn f [] \"abc)", 100).unwrap_err();
    assert!(
        e.msg.contains("unterminated string"),
        "unexpected message: {}",
        e.msg
    );
}

#[test]
fn empty_input_formats_to_single_newline() {
    assert_eq!(format_source("", 100).unwrap(), "\n");
    assert_eq!(format_source("  \n\n", 100).unwrap(), "\n");
}
