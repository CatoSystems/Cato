//! S6: the kernel expander + typed-IR builder vs the seed, differentially.
//!
//! The expander test (mode `ast`) serializes the *expanded* forms with the
//! S5 node grammar; the oracle is `read_all_spanned` + `expand_all_located`
//! over the same source. `doseq` is the one seed-only macro (outside the
//! kernel contract), so corpus files containing it are skipped — everything
//! else under `examples/` must expand identically.
//!
//! The build test (the typed-IR dump) lands with the lowering slices; the
//! oracle serializes `build_program_located` output with the same wire
//! grammar the kernel's `dump.cato` emits.

use cato_core::ast::Expr;
use cato_core::reader::read_all_spanned;
use std::process::Command;

/// Canonical concatenation order: the S4 lib, the S5 reader (+ its dump
/// serializers), then the S6 files, then the driver that defines `main`.
const CONCAT_FILES: &[&str] = &[
    "lib/sym",
    "lib/strbuf",
    "lib/span",
    "lib/ast",
    "lib/alist",
    "lib/result",
    "reader/read",
    "reader/dump",
    "hir/expand",
    "hir/hir_ref",
];

struct Paths {
    root: std::path::PathBuf,
    concat: std::path::PathBuf,
    exe: std::path::PathBuf,
}

fn setup() -> Paths {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    let cato = root.join("target/debug/cato");

    let mut src = String::new();
    for name in CONCAT_FILES {
        let p = root.join("selfhost/cato").join(format!("{}.cato", name));
        let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {}", name, e));
        src.push_str(&body);
    }
    let concat = root.join("target/selfhost_hir.cato");
    std::fs::write(&concat, &src).expect("write concat");

    let ll = root.join("target/selfhost_hir.ll");
    let exe = root.join("target/selfhost_hir_bin");
    let status = Command::new(&cato)
        .arg(&concat)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&ll)
        .status()
        .expect("cato emit");
    assert!(status.success(), "seed failed to compile the kernel hir");
    let status = Command::new("clang")
        .arg(&ll)
        .arg(root.join("runtime/runtime.c"))
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("clang");
    assert!(status.success(), "clang rejected the kernel hir's IR");

    Paths { root, concat, exe }
}

impl Paths {
    /// Every `examples/` file plus every S6 fixture. `doseq`-using files
    /// are skipped: the seed expands it, the kernel rejects it by contract.
    fn expander_corpus(&self) -> Vec<std::path::PathBuf> {
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in [
            self.root.join("examples"),
            self.root.join("selfhost/cato/hir/fixtures"),
        ] {
            for f in walk(&dir) {
                let src = std::fs::read_to_string(&f).unwrap_or_default();
                if !src.contains("doseq") {
                    files.push(f);
                }
            }
        }
        files.sort();
        assert!(!files.is_empty(), "corpus is empty");
        files
    }

    fn run_kernel(&self, file: &std::path::Path, mode: &str) -> String {
        let out = Command::new(&self.exe).arg(file).arg(mode).output().expect("kernel run");
        assert!(
            out.status.success(),
            "kernel failed on {}: {}",
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
// The oracle: seed reader + seed expander -> the S5 wire format
// ---------------------------------------------------------------------------

/// (line, col) of byte offset `lo`: 1-based line, 0-based byte column.
fn line_col(src: &str, lo: usize) -> (usize, usize) {
    let before = &src.as_bytes()[..lo];
    let line = 1 + before.iter().filter(|&&b| b == b'\n').count();
    let col = match before.iter().rposition(|&b| b == b'\n') {
        Some(p) => lo - (p + 1),
        None => lo,
    };
    (line, col)
}

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

/// The oracle's rendering: one line per top-level form — the ORIGINAL
/// byte span (expansion preserves the form count and order) plus the
/// expanded tree — or the expansion error with its span and the raw seed
/// message (the driver prints raw messages; `locate` decorates).
fn oracle_ast(src: &str) -> String {
    let (forms, spans) = match read_all_spanned(src) {
        Ok(r) => r,
        Err(e) => {
            let (l, c) = line_col(src, e.span.lo);
            return format!("err @{}:{}+{} {}\n", l, c, e.span.hi - e.span.lo, e.msg);
        }
    };
    // Per-form expansion, so an expansion error is attributed to its own
    // top-level form's span (expand_all_located's rule, minus the
    // diagnostic decoration the driver doesn't print).
    let mut out = String::new();
    for (e, sp) in forms.iter().zip(spans.iter()) {
        match cato_core::expand::expand_all(std::slice::from_ref(e)) {
            Ok(expanded) => {
                let (l, c) = line_col(src, sp.lo);
                out.push_str(&format!(
                    "@{}:{}+{}{}\n",
                    l,
                    c,
                    sp.hi - sp.lo,
                    ser(&expanded[0])
                ));
            }
            Err(msg) => {
                let (l, c) = line_col(src, sp.lo);
                return format!("err @{}:{}+{} {}\n", l, c, sp.hi - sp.lo, msg);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

#[test]
fn kernel_expander_matches_the_seed_over_the_whole_corpus() {
    let paths = setup();
    for file in paths.expander_corpus() {
        let src =
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {}: {}", file.display(), e));
        let want = oracle_ast(&src);
        let got = paths.run_kernel(&file, "ast");
        assert_eq!(
            got, want,
            "kernel expander disagrees with the seed on {}",
            file.display()
        );
    }
}
