//! S5: the kernel reader (`selfhost/cato/reader/`) vs the seed reader,
//! differentially. The oracle serializes `cato_core::reader::
//! read_all_spanned` output — top-level byte spans as `@line:col+len`,
//! then the tagged node tree — and the kernel driver (`reader_ref.cato`,
//! which `require`s the lib and reader modules) must print byte-identical
//! output for every file under `examples/` and every fixture.
//!
//! The kernel runs through the clang-compiled LLVM backend here (native
//! frames make the O(tokens) scan depth a non-issue); interp↔LLVM parity
//! of the same driver is asserted over the fixture corpus.

use cato_core::ast::Expr;
use cato_core::reader::read_all_spanned;
use std::process::Command;

struct Paths {
    root: std::path::PathBuf,
    entry: std::path::PathBuf,
    exe: std::path::PathBuf,
}

fn setup() -> Paths {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    let cato = root.join("target/debug/cato");

    let entry = root.join("selfhost/cato/reader/reader_ref.cato");
    let ll = root.join("target/selfhost_reader.ll");
    let exe = root.join("target/selfhost_reader_bin");
    let status = Command::new(&cato)
        .arg("compile")
        .arg(&entry)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&ll)
        .status()
        .expect("cato emit");
    assert!(status.success(), "seed failed to compile the kernel reader");
    let status = Command::new("clang")
        .arg(&ll)
        .arg(root.join("runtime/runtime.c"))
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("clang");
    assert!(status.success(), "clang rejected the kernel reader's IR");

    Paths { root, entry, exe }
}

impl Paths {
    fn corpus(&self) -> Vec<std::path::PathBuf> {
        let mut files: Vec<std::path::PathBuf> = walk(&self.root.join("examples"));
        files.extend(walk(&self.root.join("selfhost/cato/reader/fixtures")));
        files.sort();
        assert!(!files.is_empty(), "corpus is empty");
        files
    }

    fn run_kernel(&self, file: &std::path::Path) -> String {
        let out = Command::new(&self.exe).arg(file).output().expect("kernel run");
        assert!(
            out.status.success(),
            "kernel reader failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn run_kernel_interp(&self, file: &std::path::Path) -> String {
        let out = Command::new(self.root.join("target/debug/cato-interp-run"))
            .arg(&self.entry)
            .arg(file)
            .output()
            .expect("interp run");
        assert!(
            out.status.success(),
            "interp failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().map(|x| x == "cato").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The oracle: seed reader -> the wire format dump.cato emits
// ---------------------------------------------------------------------------

/// (line, col) of byte offset `lo`: 1-based line, 0-based byte column —
/// the same counters the kernel scan keeps as it consumes bytes.
fn line_col(src: &str, lo: usize) -> (usize, usize) {
    let before = &src.as_bytes()[..lo];
    let line = 1 + before.iter().filter(|&&b| b == b'\n').count();
    let col = match before.iter().rposition(|&b| b == b'\n') {
        Some(p) => lo - (p + 1),
        None => lo,
    };
    (line, col)
}

/// The four-byte escape pass of dump.cato's `esc-str`, byte for byte.
fn esc(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    for &c in s.as_bytes() {
        match c {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => out.push(c),
        }
    }
    String::from_utf8(out).expect("escape preserved utf-8")
}

fn ser(e: &Expr) -> String {
    match e {
        Expr::LitI64(v) => format!("(int {})", v),
        Expr::LitF64(v) => format!("(f64 {})", cato_interp::fmt_f64(*v)),
        Expr::LitBool(b) => format!("(bool {})", if *b { 1 } else { 0 }),
        Expr::LitStr(s) => format!("(str {})", esc(s)),
        Expr::LitSym(s) => format!("(sym {})", s),
        Expr::List(xs) => format!("(list{})", kids(xs)),
        Expr::Vector(xs) => format!("(vec{})", kids(xs)),
        Expr::SetLit(xs) => format!("(set{})", kids(xs)),
        Expr::MapLit(pairs) => {
            let mut out = String::from("(map");
            for (k, v) in pairs {
                out.push(' ');
                out.push_str(&ser(k));
                out.push(' ');
                out.push_str(&ser(v));
            }
            out.push(')');
            out
        }
    }
}

fn kids(xs: &[Expr]) -> String {
    let mut out = String::new();
    for x in xs {
        out.push(' ');
        out.push_str(&ser(x));
    }
    out
}

/// The oracle's rendering of one file: one line per top-level form with its
/// byte span rendered as @line:col+len, or the reader error with its span.
fn oracle(src: &str) -> String {
    match read_all_spanned(src) {
        Ok((forms, spans)) => {
            let mut out = String::new();
            for (e, sp) in forms.iter().zip(spans.iter()) {
                let (l, c) = line_col(src, sp.lo);
                out.push_str(&format!("@{}:{}+{}{}\n", l, c, sp.hi - sp.lo, ser(e)));
            }
            out
        }
        Err(e) => {
            let (l, c) = line_col(src, e.span.lo);
            format!("err @{}:{}+{} {}\n", l, c, e.span.hi - e.span.lo, e.msg)
        }
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

#[test]
fn kernel_reader_matches_the_seed_over_the_whole_corpus() {
    let paths = setup();
    for file in paths.corpus() {
        let src = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("read {}: {}", file.display(), e));
        let want = oracle(&src);
        let got = paths.run_kernel(&file);
        assert_eq!(got, want, "kernel reader disagrees with the seed on {}", file.display());
    }
}

#[test]
fn kernel_reader_backend_parity_over_the_fixtures() {
    let paths = setup();
    for file in walk(&paths.root.join("selfhost/cato/reader/fixtures")) {
        let native = paths.run_kernel(&file);
        let interp = paths.run_kernel_interp(&file);
        assert_eq!(
            native, interp,
            "backend parity mismatch on {}",
            file.display()
        );
    }
}
