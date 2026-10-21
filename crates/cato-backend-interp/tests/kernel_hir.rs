//! S6: the kernel expander + typed-IR builder vs the seed, differentially.
//!
//! The expander test (mode `ast`) serializes the *expanded* forms with the
//! S5 node grammar; the oracle is `read_all_spanned` + `expand_all_located`
//! over the same source. `doseq` is the one seed-only macro (outside the
//! kernel contract), so corpus files containing it are skipped — everything
//! else under `examples/` must expand identically.
//!
//! The build test (mode `hir`) runs the full kernel pipeline — expand →
//! build (forms → lower → moveck) → dump — over the S6 fixture corpus and
//! byte-compares the wire dump against a Rust oracle serializing the
//! seed's `build_program_located` (pre-optimization, pre-fusion) output
//! with the same grammar.

use cato_core::ast::Expr;
use cato_core::reader::read_all_spanned;
use std::process::Command;

#[derive(Clone)]
struct Paths {
    root: std::path::PathBuf,
    bin: std::path::PathBuf,
}

/// Build the kernel driver once and share the paths with every test in this
/// binary. Both tests call `setup()`, and `cargo test` runs them on parallel
/// threads: rebuilding the same `-o` path per call would let one test rewrite
/// (or truncate) the executable while the other is running it, which surfaces
/// as a spurious "No such file or directory" from `Command::output`.
fn setup() -> Paths {
    use std::sync::OnceLock;
    static PATHS: OnceLock<Paths> = OnceLock::new();
    PATHS
        .get_or_init(|| {
            let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let root = manifest.parent().unwrap().parent().unwrap().to_path_buf();
            let cato = root.join("target/debug/cato");

            // The S6 driver (`hir_ref.cato`) `require`s the lib, reader and hir
            // modules; its `main` defines the only entry point.
            let entry = root.join("selfhost/cato/hir/hir_ref.cato");

            // The driver is compiled natively once (the same kernel source the
            // interpreter would run, ~100x faster per fixture).
            let bin = root.join("target/selfhost_hir_ref");
            let status = Command::new(&cato)
                .arg("compile")
                .arg(&entry)
                .arg("-o")
                .arg(&bin)
                .status()
                .expect("cato compile");
            assert!(
                status.success(),
                "seed failed to compile the kernel hir driver"
            );

            Paths { root, bin }
        })
        .clone()
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
        let out = Command::new(&self.bin)
            .arg(file)
            .arg(mode)
            .output()
            .expect("kernel run");
        assert!(
            out.status.success(),
            "kernel failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

/// The differing lines of two dumps (one top-level form per line), as
/// `kernel:`/`seed:` pairs: whole dumps are too long to compare by eye.
fn line_diff(got: &str, want: &str) -> String {
    let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    let mut out = String::new();
    for i in 0..g.len().max(w.len()) {
        let (a, b) = (
            g.get(i).copied().unwrap_or("<none>"),
            w.get(i).copied().unwrap_or("<none>"),
        );
        if a != b {
            out.push_str(&format!("kernel: {}\nseed:   {}\n", a, b));
        }
    }
    out
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
        let src = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("read {}: {}", file.display(), e));
        let want = oracle_ast(&src);
        let got = paths.run_kernel(&file, "ast");
        assert_eq!(
            got,
            want,
            "kernel expander disagrees with the seed on {}",
            file.display()
        );
    }
}

// ---------------------------------------------------------------------------
// The oracle: seed reader + expander + build_program_located -> the S6
// typed-IR wire format (dump.cato's grammar)
// ---------------------------------------------------------------------------

/// The seed's built-in prelude (a private const in cato-core; kept in
/// sync here). It contributes adt 0 (the `(Option a)` template) and the
/// print multimethod, which `collect_generics` strips.
const PRELUDE: &str = r#"
(deftype (Option a)
  (Some [v :- a])
  (None))
(defgeneric print [x])
(defmethod print [x :- i64] (print-i64 x))
(defmethod print [x :- f64] (print-f64 x))
(defmethod print [x :- str] (print-string x))
(defmethod print [x :- bool] (print-i64 x))
(defmethod print [x :- i8] (print-i64 x))
(defmethod print [x :- i16] (print-i64 x))
(defmethod print [x :- i32] (print-i64 x))
(defmethod print [x :- u8] (print-i64 x))
(defmethod print [x :- u16] (print-i64 x))
(defmethod print [x :- u32] (print-i64 x))
(defmethod print [x :- u64] (print-i64 x))
(defmethod print [x :- usize] (print-i64 x))
"#;

fn ty_name(t: &cato_core::hir::Ty) -> String {
    use cato_core::hir::Ty;
    match t {
        Ty::I64 => "i64".into(),
        Ty::I32 => "i32".into(),
        Ty::I8 => "i8".into(),
        Ty::I16 => "i16".into(),
        Ty::U8 => "u8".into(),
        Ty::U16 => "u16".into(),
        Ty::U32 => "u32".into(),
        Ty::U64 => "u64".into(),
        Ty::Usize => "usize".into(),
        Ty::F64 => "f64".into(),
        Ty::F32 => "f32".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "str".into(),
        Ty::Map => "map".into(),
        Ty::Set => "set".into(),
        Ty::Vec(e) => format!("(Vector {})", elem_name(e)),
        Ty::Adt(i) => format!("(Adt {})", i),
        Ty::Fn(i) => format!("(Fn {})", i),
        Ty::Param(i) => format!("(Param {})", i),
    }
}

fn elem_name(e: &cato_core::hir::Elem) -> String {
    use cato_core::hir::Elem;
    match e {
        Elem::I64 => "i64".into(),
        Elem::F64 => "f64".into(),
        Elem::Str => "str".into(),
        Elem::N(n) => n.name().into(),
        Elem::Param(k) => format!("(Param {})", k),
    }
}

/// The vector-element code the kernel's type encoding uses (0/1/2/100+k).
fn elem_code(e: &cato_core::hir::Elem) -> i64 {
    use cato_core::hir::Elem;
    match e {
        Elem::I64 => 0,
        Elem::F64 => 1,
        Elem::Str => 2,
        // Seed-only narrow elements (outside the kernel subset): distinct
        // codes that cannot collide with the kernel's 0/1/2/100+k.
        Elem::N(n) => 10 + *n as i64,
        Elem::Param(k) => 100 + *k as i64,
    }
}

fn opt_res(res: &Option<cato_core::hir::Ty>) -> String {
    match res {
        Some(t) => ty_name(t),
        None => "none".into(),
    }
}

fn bin_op(op: &cato_core::hir::BinOp) -> i64 {
    use cato_core::hir::BinOp;
    match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        // Seed-only operators (outside the kernel subset).
        BinOp::Rem => 4,
        BinOp::Mod => 5,
        BinOp::BitAnd => 6,
        BinOp::BitOr => 7,
        BinOp::BitXor => 8,
        BinOp::Shl => 9,
        BinOp::Shr => 10,
    }
}

fn cmp_op(op: &cato_core::hir::CmpOp) -> i64 {
    use cato_core::hir::CmpOp;
    match op {
        CmpOp::Eq => 0,
        CmpOp::Ne => 1,
        CmpOp::Lt => 2,
        CmpOp::Gt => 3,
        CmpOp::Le => 4,
        CmpOp::Ge => 5,
    }
}

fn f64_g(v: f64) -> String {
    cato_interp::fmt_f64(v)
}

/// Serialize one typed-HIR expression with the S6 wire grammar.
fn ser_hir(e: &cato_core::hir::HExpr) -> String {
    use cato_core::hir::{HExpr, VLit};
    match e {
        HExpr::ConstI64(v) => format!("(const {})", v),
        HExpr::ConstF64(v) => format!("(f64lit {})", f64_g(*v)),
        HExpr::ConstB(b) => format!("(bool {})", if *b { 1 } else { 0 }),
        HExpr::Var(s) => format!("(var {})", s),
        HExpr::StrLit(s) => format!("(str {})", esc(s)),
        HExpr::VecLit(VLit::I64(vs)) => format!(
            "(veclit 0 [{}])",
            vs.iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        HExpr::VecLit(VLit::F64(vs)) => format!(
            "(veclit 1 [{}])",
            vs.iter().map(|v| f64_g(*v)).collect::<Vec<_>>().join(" ")
        ),
        HExpr::BinW(op, a, b, t) => {
            format!(
                "(binw {} {} {} {})",
                bin_op(op),
                ser_hir(a),
                ser_hir(b),
                ty_name(t)
            )
        }
        HExpr::BinF(op, a, b) => format!("(binf {} {} {})", bin_op(op), ser_hir(a), ser_hir(b)),
        HExpr::CmpW(op, a, b, t) => {
            format!(
                "(cmp {} {} {} {})",
                cmp_op(op),
                ser_hir(a),
                ser_hir(b),
                ty_name(t)
            )
        }
        HExpr::ICast(x, from, to) => {
            format!("(icast {} {} {})", ser_hir(x), ty_name(from), ty_name(to))
        }
        HExpr::Count(x) => format!("(count {})", ser_hir(x)),
        HExpr::ToStr(x, t) => format!("(tostr {} {})", ser_hir(x), ty_name(t)),
        HExpr::StrCat(args) => format!(
            "(strcat{})",
            args.iter()
                .map(ser_hir)
                .map(|s| format!(" {}", s))
                .collect::<String>()
        ),
        HExpr::Subs { s, start, end, .. } => {
            format!("(subs {} {} {})", ser_hir(s), ser_hir(start), ser_hir(end))
        }
        HExpr::Split { s, sep, .. } => format!("(split {} {})", ser_hir(s), ser_hir(sep)),
        HExpr::Join { sep, xs, .. } => format!("(join {} {})", ser_hir(sep), ser_hir(xs)),
        HExpr::StrNth { s, idx, res } => {
            format!("(nth {} {} {})", ser_hir(s), ser_hir(idx), opt_res(res))
        }
        HExpr::CharStr(x) => format!("(charstr {})", ser_hir(x)),
        HExpr::StrCmp(p, a, b) => {
            format!(
                "(strcmp {} {} {})",
                if *p { 1 } else { 0 },
                ser_hir(a),
                ser_hir(b)
            )
        }
        HExpr::StrOrd(op, a, b) => {
            format!("(strord {} {} {})", cmp_op(op), ser_hir(a), ser_hir(b))
        }
        HExpr::Get {
            src,
            idx,
            default,
            ty,
            res,
        } => match default {
            Some(d) => format!(
                "(get {} {} {} {} {})",
                ser_hir(src),
                ser_hir(idx),
                ser_hir(d),
                ty_name(ty),
                opt_res(res)
            ),
            None => format!(
                "(get {} {} {} {})",
                ser_hir(src),
                ser_hir(idx),
                ty_name(ty),
                opt_res(res)
            ),
        },
        HExpr::Conj {
            v,
            x,
            elem,
            in_place,
        } => format!(
            "(conj {} {} {} {})",
            ser_hir(v),
            ser_hir(x),
            elem_code(elem),
            if *in_place { 1 } else { 0 }
        ),
        HExpr::If(c, t, f, ty) => format!(
            "(if {} {} {} {})",
            ser_hir(c),
            ser_hir(t),
            ser_hir(f),
            ty_name(ty)
        ),
        HExpr::And(a, b) => format!("(and {} {})", ser_hir(a), ser_hir(b)),
        HExpr::Or(a, b) => format!("(or {} {})", ser_hir(a), ser_hir(b)),
        HExpr::Not(x) => format!("(not {})", ser_hir(x)),
        HExpr::Abs(x, t) => format!("(abs {} {})", ser_hir(x), ty_name(t)),
        HExpr::Min(a, b, t) => format!("(min {} {} {})", ser_hir(a), ser_hir(b), ty_name(t)),
        HExpr::Max(a, b, t) => format!("(max {} {} {})", ser_hir(a), ser_hir(b), ty_name(t)),
        HExpr::Seq(vs) => format!(
            "(seq{})",
            vs.iter()
                .map(ser_hir)
                .map(|s| format!(" {}", s))
                .collect::<String>()
        ),
        HExpr::Let(binds, body, frees, _) => {
            let mut parts = Vec::new();
            for (n, v, t) in binds {
                parts.push(format!("{} {} {}", n, ser_hir(v), ty_name(t)));
            }
            format!(
                "(let [{}] [{}] {})",
                parts.join(" "),
                frees.join(" "),
                ser_hir(body)
            )
        }
        HExpr::Loop { binds, ty, body } => {
            let mut parts = Vec::new();
            for (n, t, e) in binds {
                parts.push(format!("{} {} {}", n, ty_name(t), ser_hir(e)));
            }
            format!(
                "(loop [{}] {} {})",
                parts.join(" "),
                ty_name(ty),
                ser_hir(body)
            )
        }
        HExpr::Recur(args, _) => format!(
            "(recur{})",
            args.iter()
                .map(ser_hir)
                .map(|s| format!(" {}", s))
                .collect::<String>()
        ),
        HExpr::Ctor { adt, variant, args } => format!(
            "(ctor {} {}{})",
            adt,
            variant,
            args.iter()
                .map(ser_hir)
                .map(|s| format!(" {}", s))
                .collect::<String>()
        ),
        HExpr::Match { scrut, arms, ty } => {
            let mut out = format!("(match {} {}", ser_hir(scrut), ty_name(ty));
            for arm in arms {
                let v = match arm.variant {
                    Some(v) => v.to_string(),
                    None => "_".into(),
                };
                // The planner's arm-exit free rides the dump too: `-` when
                // the arm leaves the box to its owner, `[skip...]` when it
                // frees the box with those binders' slots nulled.
                let ef = match &arm.exit_free {
                    None => "-".to_string(),
                    Some(skip) => format!("[{}]", skip.join(" ")),
                };
                out.push_str(&format!(
                    " (arm {} [{}] {} {})",
                    v,
                    arm.binders.join(" "),
                    ser_hir(&arm.body),
                    ef
                ));
            }
            out.push(')');
            out
        }
        HExpr::Clone(x) => format!("(clone {})", ser_hir(x)),
        HExpr::DropStmt(x) => format!("(drop {})", ser_hir(x)),
        HExpr::Closure {
            sig,
            mangled,
            params,
            captures,
            capture_owns,
            body,
        } => format!(
            "(closure {} {} [{}] [{}] {})",
            sig,
            mangled,
            params
                .iter()
                .map(|(n, t)| format!("{}:{}", n, ty_name(t)))
                .collect::<Vec<_>>()
                .join(" "),
            captures
                .iter()
                .zip(capture_owns.iter())
                .map(|((n, t), o)| format!("{}:{}:{}", n, ty_name(t), if *o { 1 } else { 0 }))
                .collect::<Vec<_>>()
                .join(" "),
            ser_hir(body)
        ),
        HExpr::ClosureCall { f, args, sig } => format!(
            "(closurecall {} {}{})",
            ser_hir(f),
            sig,
            args.iter()
                .map(ser_hir)
                .map(|s| format!(" {}", s))
                .collect::<String>()
        ),
        HExpr::Call(name, args) => format!(
            "(call {}{})",
            name,
            args.iter()
                .map(ser_hir)
                .map(|s| format!(" {}", s))
                .collect::<String>()
        ),
        HExpr::Slurp(p) => format!("(slurp {})", ser_hir(p)),
        HExpr::Spit { path, data, .. } => {
            format!("(spit {} {})", ser_hir(path), ser_hir(data))
        }
        HExpr::Args => "(args)".into(),
        HExpr::Exit(x) => format!("(exit {})", ser_hir(x)),
        HExpr::ParseNum { s, int, res } => format!(
            "(parse {} {} {})",
            ser_hir(s),
            if *int { 1 } else { 0 },
            ty_name(res)
        ),
        other => panic!("oracle: out-of-contract HExpr node {:?}", other),
    }
}

/// The wire's param list: `[x:i64 y:str]`.
fn ser_params(params: &[(String, cato_core::hir::Ty)]) -> String {
    format!(
        "[{}]",
        params
            .iter()
            .map(|(n, t)| format!("{}:{}", n, ty_name(t)))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// The typed-IR dump of the seed's pre-optimization `build_program_located`
/// output, byte-matching the kernel's dump.cato grammar.
fn oracle_hir(src: &str) -> String {
    use cato_core::ast::Span;
    let (prelude_forms, _) = cato_core::reader::read_all_spanned(PRELUDE).expect("prelude reads");
    let (forms, spans) = match cato_core::reader::read_all_spanned(src) {
        Ok(r) => r,
        Err(e) => panic!("oracle: fixture does not read: {}", e.msg),
    };
    let mut all: Vec<(cato_core::Expr, Span)> = prelude_forms
        .into_iter()
        .map(|f| (f, Span::default()))
        .collect();
    all.extend(forms.into_iter().zip(spans));
    let (fs, ss): (Vec<_>, Vec<_>) = all.iter().cloned().unzip();
    let expanded = match cato_core::expand::expand_all_located(&fs, &ss) {
        Ok(e) => e,
        Err(msg) => return format!("BUILD-ERROR: {}", msg),
    };
    let located: Vec<(cato_core::Expr, Span)> = expanded.into_iter().zip(ss).collect();
    let (adts, defns, lambdas, fn_sigs) =
        // The kernel's checker does not infer parameter modes: its oracle
        // is the explicit-annotation language.
        match cato_core::hir::build_program_located_with(&located, false) {
            Ok(r) => r,
            Err(e) => return format!("BUILD-ERROR: {}", e),
        };

    let mut lines: Vec<String> = Vec::new();

    // adts: declared templates then instances (the combined table order);
    // instances carry their template index, recovered from the mangled
    // name prefix (the part before the first "__").
    let base = adts.len() - num_insts(&adts, &defns);
    for (i, a) in adts.iter().enumerate() {
        if i < base {
            let params = format!("[{}]", a.ty_params.join(" "));
            let variants = a
                .variants
                .iter()
                .map(|(vn, tys)| {
                    format!(
                        "({} [{}])",
                        vn,
                        tys.iter().map(ty_name).collect::<Vec<_>>().join(" ")
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("(adt {} {} {} [{}])", i, a.name, params, variants));
        } else {
            let tpl_name = a.name.split("__").next().unwrap_or("");
            let tpl = adts.iter().position(|x| x.name == tpl_name).unwrap_or(0);
            lines.push(format!("(adtinst {} {} {})", i, tpl, a.name));
        }
    }

    // defns: the seed's out order — plain in source order, externs, mono
    // instances (externs keep the seed's (const 0) placeholder body).
    for d in &defns {
        lines.push(format!(
            "(defn {} {} :- {} {} {})",
            d.name,
            ser_params(&d.params),
            ty_name(&d.ret),
            if d.is_extern { 1 } else { 0 },
            ser_hir(&d.body)
        ));
    }

    // lambdas: creation order; capture own bits print at the creation
    // default (the table itself carries no planned bits).
    for l in &lambdas {
        let caps = l
            .captures
            .iter()
            .map(|(n, t)| format!("{}:{}:1", n, ty_name(t)))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!(
            "(lambda {} [{}] {} :- {} {})",
            l.mangled,
            caps,
            ser_params(&l.params),
            ty_name(&l.ret),
            ser_hir(&l.body)
        ));
    }

    for (ps, r) in &fn_sigs {
        lines.push(format!(
            "(fnsig [{}] {})",
            ps.iter().map(ty_name).collect::<Vec<_>>().join(" "),
            ty_name(r)
        ));
    }

    if lines.is_empty() {
        String::new()
    } else {
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

/// The instance count: adts beyond the declared templates. The seed's
/// combined table is base + extra, and extra is nonempty only when an
/// instance was minted; the mangled-name prefix rule identifies them.
fn num_insts(adts: &[cato_core::AdtDef], _defns: &[cato_core::Defn]) -> usize {
    // An instance's name always contains "__"; declared names never do.
    adts.iter().filter(|a| a.name.contains("__")).count()
}

#[test]
fn kernel_hir_dump_matches_the_seed_over_the_fixtures() {
    let paths = setup();
    // The IR differential runs over the S6 fixture corpus only:
    // `examples/` exercises seed-only features (reduce/fusion, maps,
    // multimethods) that lower to nodes outside the kernel contract.
    let fixtures = walk(&paths.root.join("selfhost/cato/hir/fixtures"));
    let mut ran = 0;
    for file in fixtures {
        let src = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("read {}: {}", file.display(), e));
        let want = oracle_hir(&src);
        // Fixtures the seed itself rejects (expansion-error corpus files,
        // the seed-only macros fixture) are out of the IR contract.
        if want.starts_with("BUILD-ERROR") || want.is_empty() {
            continue;
        }
        let got = paths.run_kernel(&file, "hir");
        assert!(
            got == want,
            "kernel typed-IR dump disagrees with the seed on {}:\n{}",
            file.display(),
            line_diff(&got, &want)
        );
        ran += 1;
    }
    assert!(ran > 5, "the IR differential covered only {} fixtures", ran);
}
