use crate::hir::*;
use std::collections::HashMap;

/// LLVM IR emitter.
///
/// Every `reduce` (top-level or nested) is hoisted into a dedicated loop
/// function whose vector-data argument is declared `noalias nocapture
/// readonly`, with `inbounds` GEPs and `align 8` loads, so LLVM can
/// vectorize the fused loop (spec §4.7).
pub fn emit_module(adts: &[AdtDef], fns: &[Defn]) -> String {
    emit_module_fast(adts, fns, false)
}

/// `--fast`: attach vectorization metadata to every fused loop so LLVM
/// auto-vectorizes the language's pure loops.
pub fn emit_module_fast(adts: &[AdtDef], fns: &[Defn], fast: bool) -> String {
    // Signature table: callee name -> (ret, param types). Includes declared
    // defns and hoisted loop helpers.
    let mut sigs: HashMap<String, (Ty, Vec<Ty>)> = HashMap::new();
    for d in fns {
        sigs.insert(
            d.name.clone(),
            (d.ret, d.params.iter().map(|(_, t)| *t).collect()),
        );
    }

    let mut loops: Vec<LoopSpec> = Vec::new();
    let mut idmap: HashMap<String, usize> = HashMap::new();
    let mut plain: Vec<Defn> = Vec::new();
    let mut mdl = Mdl::new(adts.to_vec(), fast);

    for d in fns {
        if d.name != "main" && matches!(d.body, HExpr::Reduce(_)) {
            // Top-level loop defn: the function IS the helper. (main is
            // always emitted as a plain function so its i32 ABI is kept.)
            let reduce = match &d.body {
                HExpr::Reduce(r) => (**r).clone(),
                _ => unreachable!(),
            };
            loops.push(make_loop_spec(&mangle(&d.name), reduce, &d.params));
        } else {
            let body = rewrite_body(
                &d.body,
                &mangle(&d.name),
                &d.params,
                &mut idmap,
                &mut loops,
                &mut sigs,
            )
            .unwrap_or_else(|e| panic!("codegen: {}", e));
            let mut nd = d.clone();
            nd.body = body;
            plain.push(nd);
        }
    }

    let mut output: Vec<String> = Vec::new();

    output.push("; cato: generated LLVM IR (vertical slice)".to_string());
    output.push("%VecI64 = type { ptr, i64, i64 }".to_string());
    for adt in adts {
        let n = adt.variants.iter().map(|(_, f)| f.len()).max().unwrap_or(0);
        output.push(format!("%{} = type {{ i64, [{} x i64] }}", adt.name, n));
    }
    output.push(String::new());
    output.push("declare void @cato_print_f64(double)".to_string());
    output.push("declare void @cato_print_i64(i64)".to_string());
    output.push("declare void @cato_print_string(ptr)".to_string());
    output.push(String::new());

    for spec in &loops {
        output.push(String::new());
        output.push(emit_loop(spec, &mut mdl, &sigs));
    }
    for defn in &plain {
        output.push(String::new());
        if defn.is_extern {
            output.push(emit_extern_decl(defn, &mut mdl));
        } else {
            output.push(emit_plain(defn, &mut mdl, &sigs));
        }
    }

    if fast {
        output.push(String::new());
        output.push("!0 = distinct !{!0, !1}".to_string());
        output.push("!1 = !{!\"llvm.loop.vectorize.enable\", i1 true}".to_string());
    }

    // Runtime-helper declares minted during emission belong at module top.
    for d in &mdl.decls {
        let i = output
            .iter()
            .position(|l| l.starts_with("declare void @cato_print_i64"))
            .unwrap_or(0);
        output.insert(i + 1, d.clone());
        output.insert(i + 1, String::new());
    }

    // Vec-literal globals minted during emission belong at module top.
    for g in mdl.globals() {
        let i = output
            .iter()
            .position(|l| l.starts_with("declare void @cato_print_i64"))
            .unwrap_or(0);
        output.insert(i + 1, g.clone());
        output.insert(i + 1, String::new());
    }

    output.join("\n")
}

// ---------------------------------------------------------------------------
// Reduce hoisting
// ---------------------------------------------------------------------------

struct LoopSpec {
    helper: String,
    acc_ty: Ty, // I64 or F64, from the init literal
    caps: Vec<(String, Ty)>,
    /// Element type of each base vector, in call-site argument order.
    base_elems: Vec<Elem>,
    reduce: HReduce,
}

/// Element type of each base vector of a loop source (argument order).
fn source_base_elems(src: &HSource, params: &[(String, Ty)]) -> Vec<Elem> {
    match src {
        HSource::Var(v) => vec![params
            .iter()
            .find(|(n, _)| n == v)
            .map(|(_, t)| match t {
                Ty::Vec(e) => *e,
                _ => Elem::I64,
            })
            .unwrap_or(Elem::I64)],
        HSource::Lit(vl) => vec![match vl {
            VLit::I64(_) => Elem::I64,
            VLit::F64(_) => Elem::F64,
        }],
        HSource::Zip(a, b) => {
            let mut out = source_base_elems(a, params);
            out.extend(source_base_elems(b, params));
            out
        }
        HSource::Range(..) => Vec::new(),
        HSource::Take(_, inner) | HSource::Drop(_, inner) => source_base_elems(inner, params),
    }
}

fn make_loop_spec(helper: &str, reduce: HReduce, params: &[(String, Ty)]) -> LoopSpec {
    let (acc_ty, mut free) = loop_free_vars(&reduce);
    let mut caps: Vec<(String, Ty)> = free
        .vars
        .drain(..)
        .map(|v| {
            let t = params
                .iter()
                .find(|(n, _)| *n == v)
                .map(|(_, t)| *t)
                .unwrap_or(Ty::F64);
            (v, t)
        })
        .collect();
    caps.sort_by(|a, b| a.0.cmp(&b.0));
    let base_elems = source_base_elems(reduce_src(&reduce), params);
    LoopSpec {
        helper: helper.to_string(),
        acc_ty,
        caps,
        base_elems,
        reduce,
    }
}

/// Free variables contributed by a loop source: vector-variable names for
/// `Var`/`Zip` bases (passed as data/len args, not captures) are excluded,
/// while `range`/`take`/`drop` bound expressions are ordinary captures.
fn collect_source_vars(src: &HSource, bv: &mut VarSet, skip: &mut Vec<String>) {
    match src {
        HSource::Var(v) => {
            if !skip.contains(v) {
                bv.insert_expr(&HExpr::Var(v.clone()));
            }
        }
        HSource::Lit(_) => {}
        HSource::Range(a, b) => {
            bv.insert_expr(a);
            bv.insert_expr(b);
        }
        HSource::Zip(a, b) => {
            collect_source_vars(a, bv, skip);
            collect_source_vars(b, bv, skip);
        }
        HSource::Take(n, inner) | HSource::Drop(n, inner) => {
            bv.insert_expr(n);
            collect_source_vars(inner, bv, skip);
        }
    }
}

/// Call-site argument expressions for a loop source's base vectors, in
/// parameter order (one vec per Var/Lit base; Zip contributes both sides).
fn source_base_args(src: &HSource) -> Vec<HExpr> {
    match src {
        HSource::Var(v) => vec![HExpr::Var(v.clone())],
        HSource::Lit(vs) => vec![HExpr::VecLit(vs.clone())],
        HSource::Zip(a, b) => {
            let mut out = source_base_args(a);
            out.extend(source_base_args(b));
            out
        }
        HSource::Range(..) => Vec::new(),
        HSource::Take(_, inner) | HSource::Drop(_, inner) => source_base_args(inner),
    }
}

/// Vector-variable names at the base of the source — these become the
/// helper's data/len parameters rather than captures.
fn base_vec_vars(src: &HSource, out: &mut Vec<String>) {
    match src {
        HSource::Var(v) => {
            if !out.contains(v) {
                out.push(v.clone());
            }
        }
        HSource::Zip(a, b) => {
            base_vec_vars(a, out);
            base_vec_vars(b, out);
        }
        HSource::Lit(_) | HSource::Range(..) => {}
        HSource::Take(_, inner) | HSource::Drop(_, inner) => base_vec_vars(inner, out),
    }
}

fn loop_free_vars(reduce: &HReduce) -> (Ty, VarSet) {
    let (src, maps, filters, closure) = match reduce {
        HReduce::Pipeline {
            src, maps, filters, ..
        } => (src, maps, filters, None),
        HReduce::Closure {
            src,
            maps,
            filters,
            acc,
            el,
            el2,
            body,
            ..
        } => (src, maps, filters, Some((acc, el, el2, body))),
    };
    let mut base_vars = Vec::new();
    base_vec_vars(src, &mut base_vars);
    let mut bv = VarSet::new();
    collect_source_vars(src, &mut bv, &mut base_vars);
    if let Some(init) = reduce_init(reduce) {
        bv.insert_expr(init);
    }
    for m in maps {
        bv.insert_expr(&m.body);
        bv.remove(&m.el);
        if let Some(e2) = &m.el2 {
            bv.remove(e2);
        }
    }
    for fl in filters {
        bv.insert_expr(&fl.body);
        bv.remove(&fl.el);
        if let Some(e2) = &fl.el2 {
            bv.remove(e2);
        }
    }
    if let Some((acc, el, el2, body)) = closure {
        bv.insert_expr(body);
        bv.remove(acc);
        bv.remove(el);
        if let Some(e2) = el2 {
            bv.remove(e2);
        }
    }
    let t = if reduce_init(reduce).map(init_is_f64).unwrap_or(false) {
        Ty::F64
    } else {
        Ty::I64
    };
    (t, bv)
}

fn reduce_init(r: &HReduce) -> Option<&HExpr> {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => Some(init),
    }
}

/// An f64 accumulator: an f64 init literal or a promoted int init
/// (`ConvF64` wrapper inserted by HIR when elements are f64).
fn acc_ret_is_f64(r: &HReduce) -> bool {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => init_is_f64(init),
    }
}

fn init_is_f64(init: &HExpr) -> bool {
    match init {
        HExpr::ConstF64(_) => true,
        HExpr::ConvF64(_) => true,
        _ => false,
    }
}

/// Replace every `reduce` node with a call to its hoisted helper, collecting
/// the helpers. Returns the rewritten expression.
fn rewrite_body(
    h: &HExpr,
    fnprefix: &str,
    params: &[(String, Ty)],
    idmap: &mut HashMap<String, usize>,
    loops: &mut Vec<LoopSpec>,
    sigs: &mut HashMap<String, (Ty, Vec<Ty>)>,
) -> Result<HExpr, String> {
    Ok(match h {
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
        HExpr::Bin(op, a, b) => HExpr::Bin(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(rewrite_body(
            e, fnprefix, params, idmap, loops, sigs,
        )?)),
        HExpr::Count(e) => {
            HExpr::Count(Box::new(rewrite_body(e, fnprefix, params, idmap, loops, sigs)?))
        }
        HExpr::If(c, t, e, ty) => HExpr::If(
            Box::new(rewrite_body(c, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(t, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(e, fnprefix, params, idmap, loops, sigs)?),
            *ty,
        ),
        HExpr::And(a, b) => HExpr::And(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Or(a, b) => HExpr::Or(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Not(v) => HExpr::Not(Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?)),
        HExpr::Cmp(op, a, b, f) => HExpr::Cmp(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *f,
        ),
        HExpr::Abs(v, t) => {
            HExpr::Abs(Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?), *t)
        }
        HExpr::Min(a, b, t) => HExpr::Min(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::Max(a, b, t) => HExpr::Max(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::Call(name, args) => HExpr::Call(
            name.clone(),
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Let(binds, body) => HExpr::Let(
            binds
                .iter()
                .map(|(n, e, t)| {
                    Ok((
                        n.clone(),
                        rewrite_body(e, fnprefix, params, idmap, loops, sigs)?,
                        *t,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?,
            Box::new(rewrite_body(body, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Seq(vs) => HExpr::Seq(
            vs.iter()
                .map(|v| rewrite_body(v, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Reduce(r) => {
            let id = idmap.entry(fnprefix.to_string()).or_insert(0);
            let helper = format!("{}_reduce_{}", fnprefix, id);
            *id += 1;

            let ret_f64 = acc_ret_is_f64(r);
            let ret_ty = if ret_f64 { Ty::F64 } else { Ty::I64 };
            let spec = make_loop_spec(&helper, (**r).clone(), params);
            // Base sources become data/len pairs; captures follow.
            let base_args = source_base_args(reduce_src(r));
            let mut arg_tys: Vec<Ty> = Vec::new();
            for _ in &base_args {
                arg_tys.push(VEC_I64);
            }
            for (_, t) in &spec.caps {
                arg_tys.push(*t);
            }
            sigs.insert(helper.clone(), (ret_ty, arg_tys));
            loops.push(spec);

            let mut args: Vec<HExpr> = base_args;

            let caps = &loops.last().unwrap().caps.clone();
            for (cname, _) in caps {
                args.push(HExpr::Var(cname.clone()));
            }
            HExpr::Call(helper, args)
        }
        HExpr::Ctor { adt, variant, args } => HExpr::Ctor {
            adt: *adt,
            variant: *variant,
            args: args
                .iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        },
        HExpr::StrLit(s) => HExpr::StrLit(s.clone()),
        HExpr::StrCmp(p, a, b) => HExpr::StrCmp(
            *p,
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Clone(inner) => {
            HExpr::Clone(Box::new(rewrite_body(inner, fnprefix, params, idmap, loops, sigs)?))
        }
        HExpr::DropStmt(inner) => HExpr::DropStmt(Box::new(rewrite_body(
            inner, fnprefix, params, idmap, loops, sigs,
        )?)),
        HExpr::Get {
            src,
            idx,
            default,
            ty,
        } => HExpr::Get {
            src: Box::new(rewrite_body(src, fnprefix, params, idmap, loops, sigs)?),
            idx: Box::new(rewrite_body(idx, fnprefix, params, idmap, loops, sigs)?),
            default: match default {
                Some(d) => Some(Box::new(rewrite_body(
                    d, fnprefix, params, idmap, loops, sigs,
                )?)),
                None => None,
            },
            ty: *ty,
        },
        HExpr::Match { scrut, arms, ty } => HExpr::Match {
            scrut: Box::new(rewrite_body(scrut, fnprefix, params, idmap, loops, sigs)?),
            arms: arms
                .iter()
                .map(|arm| {
                    Ok(HArm {
                        variant: arm.variant,
                        binders: arm.binders.clone(),
                        body: rewrite_body(&arm.body, fnprefix, params, idmap, loops, sigs)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            ty: *ty,
        },
    })
}

// ---------------------------------------------------------------------------
// Module buffer
// ---------------------------------------------------------------------------

struct Mdl {
    vec_globals: Vec<String>,
    adts: Vec<AdtDef>,
    counter: usize,
    /// Emit `llvm.loop.vectorize.enable` metadata on fused loops (--fast).
    fast: bool,
    /// Deduplicated `declare` lines emitted lazily (runtime helpers and
    /// user externs share the namespace).
    decls: Vec<String>,
}

impl Mdl {
    fn new(adts: Vec<AdtDef>, fast: bool) -> Self {
        Mdl {
            vec_globals: Vec::new(),
            adts,
            counter: 0,
            fast,
            decls: Vec::new(),
        }
    }
    /// Emit a `declare` once per symbol (deduplicated by line).
    fn declare(&mut self, line: String) {
        if !self.decls.contains(&line) {
            self.decls.push(line);
        }
    }
    fn adt_name(&self, idx: u32) -> String {
        format!("%{}", self.adts[idx as usize].name)
    }
    /// LLVM scalar/pointer type for a Ty in value position. ADT values are
    /// always `ptr` to a stack slot (the named struct is only used for
    /// alloca/GEP), so signatures stay pointer-based — no boxing, no heap.
    fn ll_ty(&self, t: Ty) -> String {
        match t {
            Ty::I64 => "i64".into(),
            Ty::F64 => "double".into(),
            Ty::Bool => "i1".into(),
            Ty::Vec(_) => panic!("codegen: vector type in scalar position"),
            Ty::Adt(_) | Ty::Str => "ptr".into(),
            Ty::Param(_) => panic!("codegen: unresolved type parameter"),
        }
    }
    fn mint_vec(&mut self, lit: &VLit) -> (String, String, Elem) {
        let id = self.counter;
        self.counter += 1;
        let label = format!("@g_vec_{}", id);
        match lit {
            VLit::I64(vs) => {
                let items: Vec<String> = vs.iter().map(|v| format!("i64 {}", v)).collect();
                self.vec_globals.push(format!(
                    "{} = private constant [{} x i64] [{}]",
                    label,
                    vs.len(),
                    items.join(", ")
                ));
            }
            VLit::F64(vs) => {
                let items: Vec<String> = vs
                    .iter()
                    .map(|v| format!("double {}", llvm_f64(*v)))
                    .collect();
                self.vec_globals.push(format!(
                    "{} = private constant [{} x double] [{}]",
                    label,
                    vs.len(),
                    items.join(", ")
                ));
            }
        }
        let elem = match lit {
            VLit::I64(_) => Elem::I64,
            VLit::F64(_) => Elem::F64,
        };
        let len = match lit {
            VLit::I64(vs) => vs.len(),
            VLit::F64(vs) => vs.len(),
        };
        (label, len.to_string(), elem)
    }
    /// Mint a private NUL-terminated `[N x i8]` constant for a string
    /// literal; returns the global label.
    fn mint_str(&mut self, s: &str) -> String {
        let id = self.counter;
        self.counter += 1;
        let label = format!("@g_str_{}", id);
        let mut items: Vec<String> = s
            .bytes()
            .map(|b| format!("i8 {}", b))
            .collect();
        items.push("i8 0".to_string()); // NUL terminator
        self.vec_globals.push(format!(
            "{} = private unnamed_addr constant [{} x i8] [{}]",
            label,
            items.len(),
            items.join(", ")
        ));
        label
    }
    /// LLVM type of a vector element.
    fn elem_ll(&self, e: Elem) -> &'static str {
        match e {
            Elem::I64 => "i64",
            Elem::F64 => "double",
            Elem::Param(_) => panic!("codegen: unresolved element type parameter"),
        }
    }
    fn globals(&self) -> &[String] {
        &self.vec_globals
    }
}

// ---------------------------------------------------------------------------
// Function emitter
// ---------------------------------------------------------------------------

struct FnCtx {
    counter: usize,
    blk: usize,
    cur: String,
    lines: Vec<String>,
    /// Allocas hoisted to the function entry prelude (loop-safe ADT slots).
    prelude: Vec<String>,
    scalar: HashMap<String, (String, Ty)>,
    vecs: HashMap<String, (String, String, Elem)>,
}

impl FnCtx {
    fn new() -> Self {
        FnCtx {
            counter: 0,
            blk: 0,
            cur: String::new(),
            lines: Vec::new(),
            prelude: Vec::new(),
            scalar: HashMap::new(),
            vecs: HashMap::new(),
        }
    }
    fn temp(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("%t{}", n)
    }
    fn block_name(&mut self) -> String {
        let n = self.blk;
        self.blk += 1;
        format!("b{}", n)
    }
    fn open(&mut self, label: &str) -> String {
        let l = label.to_string();
        self.cur = l.clone();
        l
    }
    fn line(&mut self, s: &str) {
        self.lines.push(s.to_string());
    }
    fn clone_with_lines(&self, lines: Vec<String>) -> Self {
        FnCtx {
            counter: self.counter,
            blk: self.blk,
            cur: self.cur.clone(),
            lines,
            prelude: self.prelude.clone(),
            scalar: self.scalar.clone(),
            vecs: self.vecs.clone(),
        }
    }
    /// Splice hoisted entry allocas directly after the `entry:` label.
    fn with_prelude(mut lines: Vec<String>, prelude: &[String]) -> Vec<String> {
        if !prelude.is_empty() {
            let pos = lines
                .iter()
                .position(|l| l == "entry:")
                .expect("entry block");
            for (k, p) in prelude.iter().enumerate() {
                lines.insert(pos + 1 + k, p.clone());
            }
        }
        lines
    }
}

/// Scalar type of a concrete vector element.
fn elem_ty_of(e: Elem) -> Ty {
    match e {
        Elem::I64 => Ty::I64,
        Elem::F64 => Ty::F64,
        Elem::Param(_) => panic!("codegen: unresolved element type parameter"),
    }
}

fn mangle(name: &str) -> String {
    name.replace('-', "_")
}

fn llvm_f64(v: f64) -> String {
    // LLVM decimal FP constants need an explicit decimal point before 'e'
    // (Rust's {:e} renders "0e0", which LLVM reads as an integer constant).
    let s = format!("{:e}", v);
    if let Some(pos) = s.find('e') {
        if s[..pos].contains('.') {
            s
        } else {
            format!("{}.0{}", &s[..pos], &s[pos..])
        }
    } else if s.contains('.') {
        s
    } else {
        format!("{}.0", s)
    }
}

// ---------------------------------------------------------------------------
// Loop function emission
// ---------------------------------------------------------------------------

/// Peel take/drop wrappers off a loop source, returning them in application
/// order (innermost first) plus the base source. `is_drop` distinguishes the
/// two wrapper kinds.
fn unwrap_source(src: &HSource) -> (Vec<(&HExpr, bool)>, &HSource) {
    let mut wrappers = Vec::new();
    let mut cur = src;
    loop {
        match cur {
            HSource::Take(n, inner) => {
                wrappers.push((&**n, false));
                cur = inner;
            }
            HSource::Drop(n, inner) => {
                wrappers.push((&**n, true));
                cur = inner;
            }
            other => {
                // Collected outer-to-inner; application is innermost first.
                wrappers.reverse();
                return (wrappers, other);
            }
        }
    }
}

fn emit_loop(spec: &LoopSpec, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    let mut adt_caps: Vec<(String, u32)> = Vec::new();
    for (n, t) in &spec.caps {
        if let Ty::Adt(i) = t {
            adt_caps.push((n.clone(), *i));
        } else {
            c.scalar.insert(n.clone(), (format!("%{}", mangle(n)), *t));
        }
    }
    let acc_ll = if spec.acc_ty == Ty::F64 { "double" } else { "i64" };
    let init_val = loop_init(&spec.reduce);
    let init_lit = if spec.acc_ty == Ty::F64 {
        llvm_f64(init_val)
    } else {
        init_val.to_string()
    };

    // Source wrappers in application order (innermost first) and the base.
    let (wrappers, base) = unwrap_source(reduce_src(&spec.reduce));

    // Helper signature: one data/len pair per base vector variable, then
    // captures. `range` bases contribute no parameters (their bounds are
    // ordinary captures evaluated in the entry block).
    let mut params: Vec<String> = Vec::new();
    match (base, spec.base_elems.len()) {
        (HSource::Var(_) | HSource::Lit(_), 1) => {
            params.push("ptr noalias nocapture readonly %data".to_string());
            params.push("i64 %len".to_string());
        }
        (HSource::Zip(..), 2) => {
            params.push("ptr noalias nocapture readonly %dataA".to_string());
            params.push("i64 %lenA".to_string());
            params.push("ptr noalias nocapture readonly %dataB".to_string());
            params.push("i64 %lenB".to_string());
        }
        (HSource::Range(..), _) => {}
        _ => unreachable!("base source / element count"),
    }
    for (n, t) in &spec.caps {
        let ll = match t {
            Ty::Adt(i) => mdl.adt_name(*i),
            other => mdl.ll_ty(*other),
        };
        params.push(format!("{} %{}", ll, mangle(n)));
    }
    for (n, i) in &adt_caps {
        let r = format!("%{}", mangle(n));
        let slot = c.temp();
        c.prelude
            .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(*i)));
        c.prelude.push(format!(
            "  store {} {}, ptr {}, align 8",
            mdl.adt_name(*i),
            r,
            slot
        ));
        c.scalar.insert(n.clone(), (slot, Ty::Adt(*i)));
    }

    c.line("entry:");
    c.open("entry");

    // Compute the iteration start index and element count. take/drop/range
    // bounds are emitted here (branchlessly), so the whole pipeline still
    // compiles to one loop.
    let (mut start_v, mut count_v): (String, String) = match base {
        HSource::Var(_) | HSource::Lit(_) => ("0".to_string(), "%len".to_string()),
        HSource::Zip(..) => {
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 %lenA, %lenB", lt));
            let cnt = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, i64 %lenA, i64 %lenB",
                cnt, lt
            ));
            ("0".to_string(), cnt)
        }
        HSource::Range(a, b) => {
            let sv = emit_int(a, &mut c, mdl, sigs);
            let ev = emit_int(b, &mut c, mdl, sigs);
            let d = c.temp();
            c.line(&format!("  {} = sub i64 {}, {}", d, ev, sv));
            let neg = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, 0", neg, d));
            let cnt = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", cnt, neg, d));
            (sv, cnt)
        }
        _ => unreachable!("base source"),
    };
    for (n, is_drop) in &wrappers {
        let nv = emit_int(n, &mut c, mdl, sigs);
        if *is_drop {
            let s1 = c.temp();
            c.line(&format!("  {} = add i64 {}, {}", s1, start_v, nv));
            let c1 = c.temp();
            c.line(&format!("  {} = sub i64 {}, {}", c1, count_v, nv));
            let neg = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, 0", neg, c1));
            let c1c = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", c1c, neg, c1));
            start_v = s1;
            count_v = c1c;
        } else {
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, {}", lt, nv, count_v));
            let c2 = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, i64 {}, i64 {}",
                c2, lt, nv, count_v
            ));
            count_v = c2;
        }
    }
    let t_isempty = c.temp();
    c.line(&format!("  {} = icmp eq i64 {}, 0", t_isempty, count_v));
    c.line(&format!("  br i1 {}, label %ret, label %loop", t_isempty));

    // Element / accumulator bindings shared by all stages.
    let (el, el2) = match &spec.reduce {
        HReduce::Closure { el, el2, .. } => (el.clone(), el2.clone()),
        HReduce::Pipeline { maps, filters, .. } => {
            let (first_el, first_el2) = match (maps.first(), filters.first()) {
                (Some(m), _) => (Some(&m.el), m.el2.clone()),
                (None, Some(f)) => (Some(&f.el), f.el2.clone()),
                (None, None) => (None, None),
            };
            (
                first_el.cloned().unwrap_or_else(|| "x".to_string()),
                first_el2,
            )
        }
    };
    let accsym = match &spec.reduce {
        HReduce::Closure { acc, .. } => acc.clone(),
        _ => "acc".to_string(),
    };

    // Register defined in the loop body that carries the accumulator to the
    // next iteration (and into `ret`). Set by each branch below.
    let carried: String;
    let carried_block: &str;

    let (filters, maps) = match &spec.reduce {
        HReduce::Closure { maps, filters, .. } | HReduce::Pipeline { maps, filters, .. } => {
            (filters, maps)
        }
    };

    // Element loads for the base kind, at index start + i.
    // Returns (%x-reg, Option(%x2-reg)).
    fn elem_loads(
        c: &mut FnCtx,
        base: &HSource,
        start_v: &str,
        elems: &[Elem],
        mdl: &Mdl,
    ) -> (String, Ty, Option<(String, Ty)>) {
        let ll = |e: Elem| mdl.elem_ll(e);
        match base {
            HSource::Var(_) | HSource::Lit(_) => {
                let e = elems.first().copied().unwrap_or(Elem::I64);
                let pidx = c.temp();
                c.line(&format!("  {} = add i64 {}, %i", pidx, start_v));
                let p = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr %data, i64 {}",
                    p,
                    ll(e),
                    pidx
                ));
                let x = c.temp();
                c.line(&format!(
                    "  {} = load {}, ptr {}, align 8",
                    x,
                    ll(e),
                    p
                ));
                (x, elem_ty_of(e), None)
            }
            HSource::Zip(..) => {
                let ea = elems.first().copied().unwrap_or(Elem::I64);
                let eb = elems.get(1).copied().unwrap_or(Elem::I64);
                let pidx = c.temp();
                c.line(&format!("  {} = add i64 {}, %i", pidx, start_v));
                let pa = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr %dataA, i64 {}",
                    pa,
                    ll(ea),
                    pidx
                ));
                let xa = c.temp();
                c.line(&format!("  {} = load {}, ptr {}, align 8", xa, ll(ea), pa));
                let pb = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr %dataB, i64 {}",
                    pb,
                    ll(eb),
                    pidx
                ));
                let xb = c.temp();
                c.line(&format!("  {} = load {}, ptr {}, align 8", xb, ll(eb), pb));
                (
                    xa,
                    elem_ty_of(ea),
                    Some((xb, elem_ty_of(eb))),
                )
            }
            HSource::Range(..) => {
                let x = c.temp();
                c.line(&format!("  {} = add i64 {}, %i", x, start_v));
                (x, Ty::I64, None)
            }
            _ => unreachable!("base source"),
        }
    }

    match &spec.reduce {
        // Single-block closure: plain body, no stages, no control flow.
        HReduce::Closure { body, .. }
            if maps.is_empty() && filters.is_empty() && !has_ctl(body) =>
        {
            // Compute the loop-carried accumulator value first (its register
            // name becomes the phi's back-edge operand), then emit the block.
            let mut scratch = c.clone_with_lines(Vec::new());
            scratch.cur = "%loop".to_string();
            let (xv, xty, x2v) = elem_loads(&mut scratch, base, &start_v, &spec.base_elems, mdl);
            scratch.scalar.insert(el.clone(), (xv.clone(), xty));
            if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                scratch.scalar.insert(name.clone(), (reg.clone(), *ty));
            }
            scratch
                .scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let val = if spec.acc_ty == Ty::F64 {
                emit_float(body, &mut scratch, mdl, sigs)
            } else {
                emit_int(body, &mut scratch, mdl, sigs)
            };
            c.counter = scratch.counter;
            c.line("");
            c.line("loop:");
            c.open("loop");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ {}, %loop ]",
                acc_ll, init_lit, val
            ));
            let (xv, xty, x2v) = elem_loads(&mut c, base, &start_v, &spec.base_elems, mdl);
            c.scalar.insert(el.clone(), (xv.clone(), xty));
            if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                c.scalar.insert(name.clone(), (reg.clone(), *ty));
            }
            c.scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            c.lines.extend(scratch.lines);
            carried = val;
            carried_block = "%loop";
            c.line("  %i.next = add i64 %i, 1");
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, {}", t_cond, count_v));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
        // Single-block pipeline combine: no stages, a binop combine can't fork.
        HReduce::Pipeline { combine, .. } if maps.is_empty() && filters.is_empty() => {
            let _ = combine;
            c.line("");
            c.line("loop:");
            c.open("loop");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ %acc.next, %loop ]",
                acc_ll, init_lit
            ));
            let (xv, xty, x2v) = elem_loads(&mut c, base, &start_v, &spec.base_elems, mdl);
            c.scalar.insert(el.clone(), (xv.clone(), xty));
            if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                c.scalar.insert(name.clone(), (reg.clone(), *ty));
            }
            c.scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let combined = combine_step(*combine, &xv, "%acc", spec.acc_ty, &mut c);
            c.line(&format!("  %acc.next = {}", combined));
            carried = "%acc.next".to_string();
            carried_block = "%loop";
            c.line("  %i.next = add i64 %i, 1");
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, {}", t_cond, count_v));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
        // Structured apply/skip/merge: any maps, filters, or control flow.
        _ => {
            c.line("");
            c.line("loop:");
            c.open("loop");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %merge ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ %acc.merge, %merge ]",
                acc_ll, init_lit
            ));
            let (xv, xty, x2v) = elem_loads(&mut c, base, &start_v, &spec.base_elems, mdl);

            if !filters.is_empty() {
                for fl in filters {
                    c.scalar.insert(fl.el.clone(), (xv.clone(), fl.el_ty));
                    if let (Some(name), Some((reg, ty))) = (&fl.el2, &x2v) {
                        c.scalar.insert(name.clone(), (reg.clone(), *ty));
                    }
                    let pv = emit_bool(&fl.body, &mut c, mdl, sigs);
                    let t_keep = c.temp();
                    c.line(&format!("  {} = icmp ne i1 {}, 0", t_keep, pv));
                    c.line(&format!("  br i1 {}, label %apply, label %skip", t_keep));
                }
            } else {
                c.line("  br label %apply");
            }

            c.line("");
            c.line("apply:");
            c.open("apply");
            let mut val = xv.clone();
            let mut val_ty = xty;
            for m in maps.iter() {
                c.scalar.insert(m.el.clone(), (val.clone(), m.el_ty));
                if let (Some(name), Some((reg, ty))) = (&m.el2, &x2v) {
                    c.scalar.insert(name.clone(), (reg.clone(), *ty));
                }
                val = emit_typed(&m.body, m.res_ty, &mut c, mdl, sigs);
                val_ty = m.res_ty;
            }
            c.scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let r = match &spec.reduce {
                HReduce::Closure { body, .. } => {
                    c.scalar.insert(el.clone(), (val.clone(), val_ty));
                    if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                        c.scalar.insert(name.clone(), (reg.clone(), *ty));
                    }
                    emit_typed(body, spec.acc_ty, &mut c, mdl, sigs)
                }
                HReduce::Pipeline { combine, .. } => {
                    let t = c.temp();
                    let combined =
                        combine_step(*combine, &val, "%acc", spec.acc_ty, &mut c);
                    c.line(&format!("  {} = {}", t, combined));
                    t
                }
            };
            let r = pin_value(acc_ll, &r, &mut c);
            let apply_pred = c.cur.clone();
            c.line("  br label %merge");

            c.line("");
            c.line("skip:");
            c.open("skip");
            c.line("  br label %merge");

            c.line("");
            c.line("merge:");
            c.open("merge");
            c.line(&format!(
                "  %acc.merge = phi {} [ {}, %{} ], [ %acc, %skip ]",
                acc_ll, r, apply_pred
            ));
            c.line("  %i.next = add i64 %i, 1");
            carried = "%acc.merge".to_string();
            carried_block = "%merge";
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, {}", t_cond, count_v));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
    }

    c.line("");
    c.line("ret:");
    c.open("ret");
    let t_ret = c.temp();
    c.line(&format!(
        "  {} = phi {} [ {}, %entry ], [ {}, {} ]",
        t_ret, acc_ll, init_lit, carried, carried_block
    ));
    c.line(&format!("  ret {} {}", acc_ll, t_ret));

    let mut lines = FnCtx::with_prelude(c.lines, &c.prelude);
    if mdl.fast {
        // Tag the loop backedge: LLVM then auto-vectorizes the pure fused
        // loop (spec: auto-SIMD).
        for line in lines.iter_mut() {
            if line.starts_with("  br i1 ") && line.ends_with("label %loop, label %ret") {
                *line = format!("{}, !llvm.loop !0", line);
            }
        }
    }
    let mut text = format!("define {} @{}(", acc_ll, spec.helper);
    text.push_str(&params.join(", "));
    text.push_str(") {\n");
    text.push_str(&lines.join("\n"));
    text.push('\n');
    text.push_str("}\n");
    text
}

fn loop_init(r: &HReduce) -> f64 {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => match init {
            HExpr::ConstF64(v) => *v,
            HExpr::ConstI64(v) => *v as f64,
            HExpr::ConvF64(inner) => match &**inner {
                HExpr::ConstI64(v) => *v as f64,
                _ => 0.0,
            },
            _ => 0.0,
        },
    }
}

fn combine_step(op: BinOp, val: &str, acc: &str, ty: Ty, _c: &mut FnCtx) -> String {
    let (fop, iop) = match op {
        BinOp::Add => ("fadd", "add"),
        BinOp::Sub => ("fsub", "sub"),
        BinOp::Mul => ("fmul", "mul"),
        BinOp::Div => ("fdiv", "sdiv"),
    };
    let (op, sigil) = if ty == Ty::F64 { (fop, "double") } else { (iop, "i64") };
    format!("{} {} {}, {}", op, sigil, acc, val)
}

// ---------------------------------------------------------------------------
// Plain (non-loop) function emission
// ---------------------------------------------------------------------------

fn emit_plain(defn: &Defn, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    let mut params = Vec::new();
    for (pname, pty) in &defn.params {
        match pty {
            Ty::Vec(e) => {
                let d = format!("%{}_data", mangle(pname));
                let l = format!("%{}_len", mangle(pname));
                c.vecs.insert(pname.clone(), (d.clone(), l.clone(), *e));
                params.push(format!("ptr noalias nocapture readonly {}", d));
                params.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::F64));
                params.push(format!("double {}", r));
            }
            Ty::I64 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::I64));
                params.push(format!("i64 {}", r));
            }
            Ty::Bool => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Bool));
                params.push(format!("i1 {}", r));
            }
            Ty::Str => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Str));
                params.push(format!("ptr {}", r));
            }
            Ty::Param(_) => panic!("codegen: unresolved type parameter in params"),
            Ty::Adt(i) => {
                // By-value boundary: the incoming struct is copied into an
                // entry slot; all internal uses go through the ptr.
                let r = format!("%{}", mangle(pname));
                params.push(format!("{} {}", mdl.adt_name(*i), r));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(*i)));
                c.prelude.push(format!(
                    "  store {} {}, ptr {}, align 8",
                    mdl.adt_name(*i),
                    r,
                    slot
                ));
                c.scalar.insert(pname.clone(), (slot, *pty));
            }
        }
    }

    c.line("entry:");
    c.open("entry");
    if defn.name == "main" {
        for form in seq_forms(&defn.body) {
            emit_stmt(&form, &mut c, mdl, sigs);
        }
    } else {
        let val = emit_for_ret(&defn.body, &mut c, mdl, sigs, defn.ret);
        if let Ty::Adt(i) = defn.ret {
            // Copy the result slot out by value; a returned ptr would dangle.
            let lv = c.temp();
            c.line(&format!(
                "  {} = load {}, ptr {}, align 8",
                lv,
                mdl.adt_name(i),
                val
            ));
            c.line(&format!("  ret {} {}", mdl.adt_name(i), lv));
        } else {
            let ret_ll = if defn.ret == Ty::Bool {
                "i1"
            } else if defn.ret == Ty::F64 {
                "double"
            } else {
                "i64"
            };
            c.line(&format!("  ret {} {}", ret_ll, val));
        }
    }

    let ret_ll = if defn.name == "main" {
        "i32".to_string()
    } else if defn.ret == Ty::Bool {
        "i1".to_string()
    } else if defn.ret == Ty::F64 {
        "double".to_string()
    } else if let Ty::Adt(i) = defn.ret {
        mdl.adt_name(i)
    } else {
        "i64".to_string()
    };
    let mut text = format!("define {} @{}(", ret_ll, mangle(&defn.name));
    text.push_str(&params.join(", "));
    text.push_str(") {\n");
    text.push_str(&FnCtx::with_prelude(c.lines, &c.prelude).join("\n"));
    if defn.name == "main" {
        text.push('\n');
        text.push_str("  ret i32 0\n");
    }
    text.push_str("}\n");
    text
}

/// FFI declaration: a plain LLVM `declare` — direct C calls, no marshaling.
fn emit_extern_decl(defn: &Defn, _mdl: &mut Mdl) -> String {
    let mut params = Vec::new();
    for (_, pty) in &defn.params {
        match pty {
            Ty::Vec(e) => {
                params.push(format!("ptr noalias nocapture readonly %data",));
                params.push("i64 %len".to_string());
                let _ = e;
            }
            Ty::F64 => params.push("double".to_string()),
            Ty::I64 => params.push("i64".to_string()),
            Ty::Bool => params.push("i1".to_string()),
            Ty::Str => params.push("ptr".to_string()),
            Ty::Adt(_) | Ty::Param(_) => {
                panic!("codegen: extern params must be scalars, strings or vectors")
            }
        }
    }
    let ret_ll = match defn.ret {
        Ty::I64 => "i64",
        Ty::F64 => "double",
        Ty::Bool => "i1",
        Ty::Str => "ptr",
        _ => panic!("codegen: extern return must be i64, f64, bool or str"),
    };
    let line = format!(
        "declare {} @{}({})",
        ret_ll,
        mangle(&defn.name),
        params.join(", ")
    );
    line
}

fn seq_forms(b: &HExpr) -> Vec<HExpr> {
    match b {
        HExpr::Seq(vs) => vs.clone(),
        other => vec![other.clone()],
    }
}

fn emit_for_ret(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>, ret: Ty) -> String {
    match ret {
        Ty::F64 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        Ty::I64 => emit_int(e, c, mdl, sigs),
        Ty::Adt(_) => emit_adt(e, c, mdl, sigs),
        Ty::Str => emit_str_val(e, c, mdl, sigs),
        Ty::Vec(_) => panic!("codegen: cannot return a vector"),
        Ty::Param(_) => panic!("codegen: unresolved type parameter"),
    }
}

fn emit_stmt(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
    match e {
        HExpr::Call(name, args) if name == "print-f64" => {
            let v = emit_float(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_f64(double {})", v));
        }
        HExpr::Call(name, args) if name == "print-i64" => {
            let v = emit_int(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_i64(i64 {})", v));
        }
        _ => {
            emit_for_ret(e, c, mdl, sigs, Ty::I64);
        }
    }
}

// ---------------------------------------------------------------------------
// Expression emission
// ---------------------------------------------------------------------------

fn reduce_src(r: &HReduce) -> &HSource {
    match r {
        HReduce::Pipeline { src, .. } | HReduce::Closure { src, .. } => src,
    }
}

fn has_ctl_source(src: &HSource) -> bool {
    match src {
        HSource::Var(_) | HSource::Lit(_) => false,
        HSource::Range(a, b) => has_ctl(a) || has_ctl(b),
        HSource::Zip(a, b) => has_ctl_source(a) || has_ctl_source(b),
        HSource::Take(n, inner) | HSource::Drop(n, inner) => has_ctl(n) || has_ctl_source(inner),
    }
}

/// Does this expression (or a stage / closure body) fork into basic blocks?
fn has_ctl(e: &HExpr) -> bool {
    match e {
        HExpr::If(..) | HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Match { .. } => {
            true
        }
        HExpr::Bin(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::StrLit(_) => false,
        HExpr::StrCmp(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::ConvF64(x) | HExpr::Count(x) | HExpr::Abs(x, _) => has_ctl(x),
        HExpr::Clone(x) | HExpr::DropStmt(x) => has_ctl(x),
        HExpr::Call(_, args) => args.iter().any(has_ctl),
        HExpr::Ctor { args, .. } => args.iter().any(has_ctl),
        HExpr::Get { idx, default, .. } => {
            has_ctl(idx) || default.as_deref().map(has_ctl).unwrap_or(false)
        }
        HExpr::Let(binds, body) => binds.iter().any(|(_, e, _)| has_ctl(e)) || has_ctl(body),
        HExpr::Seq(vs) => vs.iter().any(has_ctl),
        HExpr::Cmp(..) => false,
        HExpr::Var(_) | HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::VecLit(_) => false,
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline { init, maps, filters, src, .. } => {
                has_ctl(init)
                    || has_ctl_source(src)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
            HReduce::Closure { init, src, body, maps, filters, .. } => {
                has_ctl(init)
                    || has_ctl_source(src)
                    || has_ctl(body)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
        },
    }
}

/// Pin a bare literal into a virtual register before a `br`/`phi`.
fn pin_value(ll_ty: &str, r: &str, c: &mut FnCtx) -> String {
    if r.starts_with('%') {
        return r.to_string();
    }
    let t = c.temp();
    if ll_ty == "double" {
        c.line(&format!("  {} = fadd double {}, 0.000000e+00", t, r));
    } else if ll_ty == "i1" {
        c.line(&format!("  {} = xor i1 {}, false", t, r));
    } else {
        c.line(&format!("  {} = add i64 {}, 0", t, r));
    }
    t
}

fn emit_typed(
    e: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match ty {
        Ty::F64 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        Ty::I64 => emit_int(e, c, mdl, sigs),
        Ty::Adt(_) => emit_adt(e, c, mdl, sigs),
        Ty::Str => emit_str_val(e, c, mdl, sigs),
        Ty::Vec(_) => panic!("codegen: control flow cannot produce a vector"),
        Ty::Param(_) => panic!("codegen: unresolved type parameter"),
    }
}

fn emit_bool(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::StrCmp(eq, a, b) => {
            let av = emit_str_val(a, c, mdl, sigs);
            let bv = emit_str_val(b, c, mdl, sigs);
            mdl.declare("declare i32 @strcmp(ptr, ptr)".to_string());
            let t = c.temp();
            c.line(&format!("  {} = call i32 @strcmp(ptr {}, ptr {})", t, av, bv));
            let z = c.temp();
            c.line(&format!(
                "  {} = icmp {} i32 {}, 0",
                z,
                if *eq { "eq" } else { "ne" },
                t
            ));
            z
        }
        HExpr::Cmp(op, a, b, is_f64) => {
            if *is_f64 {
                let av = emit_float(a, c, mdl, sigs);
                let bv = emit_float(b, c, mdl, sigs);
                let op = match op {
                    CmpOp::Eq => "oeq",
                    CmpOp::Ne => "one",
                    CmpOp::Lt => "olt",
                    CmpOp::Gt => "ogt",
                    CmpOp::Le => "ole",
                    CmpOp::Ge => "oge",
                };
                let t = c.temp();
                c.line(&format!("  {} = fcmp {} double {}, {}", t, op, av, bv));
                t
            } else {
                let av = emit_int(a, c, mdl, sigs);
                let bv = emit_int(b, c, mdl, sigs);
                let op = match op {
                    CmpOp::Eq => "eq",
                    CmpOp::Ne => "ne",
                    CmpOp::Lt => "slt",
                    CmpOp::Gt => "sgt",
                    CmpOp::Le => "sle",
                    CmpOp::Ge => "sge",
                };
                let t = c.temp();
                c.line(&format!("  {} = icmp {} i64 {}, {}", t, op, av, bv));
                t
            }
        }
        HExpr::And(a, b) => {
            let la = emit_bool(a, c, mdl, sigs);
            let parent = c.cur.clone();
            let rhs = c.block_name();
            let merged = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", la, rhs, merged));
            let lbl = c.open(&rhs);
            c.line(&format!("{}:", lbl));
            let rb = emit_bool(b, c, mdl, sigs);
            let bp = c.cur.clone();
            c.line(&format!("  br label %{}", merged));
            let lbl = c.open(&merged);
            c.line(&format!("{}:", lbl));
            let res = c.temp();
            c.line(&format!(
                "  {} = phi i1 [ false, %{} ], [ {}, %{} ]",
                res, parent, rb, bp
            ));
            res
        }
        HExpr::Or(a, b) => {
            let la = emit_bool(a, c, mdl, sigs);
            let parent = c.cur.clone();
            let rhs = c.block_name();
            let merged = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", la, merged, rhs));
            let lbl = c.open(&rhs);
            c.line(&format!("{}:", lbl));
            let rb = emit_bool(b, c, mdl, sigs);
            let bp = c.cur.clone();
            c.line(&format!("  br label %{}", merged));
            let lbl = c.open(&merged);
            c.line(&format!("{}:", lbl));
            let res = c.temp();
            c.line(&format!(
                "  {} = phi i1 [ true, %{} ], [ {}, %{} ]",
                res, parent, rb, bp
            ));
            res
        }
        HExpr::Not(v) => {
            let iv = emit_bool(v, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = xor i1 {}, true", t, iv));
            t
        }
        HExpr::Var(v) => {
            let (r, _) = c.scalar.get(v).unwrap_or_else(|| panic!("codegen: unknown bool var {}", v));
            r.clone()
        }
        HExpr::If(cond, then, els, Ty::Bool) => emit_if_node(cond, then, els, Ty::Bool, c, mdl, sigs),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Call(name, args) if name == "even?" => {
            let v = emit_int(&args[0], c, mdl, sigs);
            let p = c.temp();
            c.line(&format!("  {} = and i64 {}, 1", p, v));
            let t = c.temp();
            c.line(&format!("  {} = icmp eq i64 {}, 0", t, p));
            t
        }
        other => panic!("codegen: expected a boolean expression, got {:?}", other),
    }
}

fn emit_if_node(
    cond: &HExpr,
    then: &HExpr,
    els: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let lc = emit_bool(cond, c, mdl, sigs);
    let bt = c.block_name();
    let be = c.block_name();
    let bm = c.block_name();
    c.line(&format!("  br i1 {}, label %{}, label %{}", lc, bt, be));
    let lbl = c.open(&bt);
    c.line(&format!("{}:", lbl));
    let tv = emit_typed(then, ty, c, mdl, sigs);
    let tp = c.cur.clone();
    c.line(&format!("  br label %{}", bm));
    let lbl = c.open(&be);
    c.line(&format!("{}:", lbl));
    let ev = emit_typed(els, ty, c, mdl, sigs);
    let ep = c.cur.clone();
    c.line(&format!("  br label %{}", bm));
    let lbl = c.open(&bm);
    c.line(&format!("{}:", lbl));
    let res = c.temp();
    let lty = match ty {
        Ty::F64 => "double".to_string(),
        Ty::Bool => "i1".to_string(),
        Ty::Adt(_) => "ptr".to_string(),
        _ => "i64".to_string(),
    };
    c.line(&format!(
        "  {} = phi {} [ {}, %{} ], [ {}, %{} ]",
        res, lty, tv, tp, ev, ep
    ));
    res
}

// ---------------------------------------------------------------------------
// ADTs: constructors and match
// ---------------------------------------------------------------------------

/// Which ADT an expression of ADT type denotes (needed to read variant tags).
fn adt_of(e: &HExpr, c: &FnCtx, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> u32 {
    match e {
        HExpr::Var(v) => match c.scalar.get(v) {
            Some((_, Ty::Adt(i))) => *i,
            _ => panic!("codegen: {} is not an ADT binding", v),
        },
        HExpr::Ctor { adt, .. } => *adt,
        HExpr::Match { ty: Ty::Adt(i), .. } => *i,
        HExpr::Call(name, _) => match sigs.get(name) {
            Some((Ty::Adt(i), _)) => *i,
            _ => panic!("codegen: call {} does not return an ADT", name),
        },
        other => panic!("codegen: expected an ADT value, got {:?}", other),
    }
}

/// Emit an expression of ADT type; the value is a `ptr` to a stack slot.
fn emit_adt(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(r, _)| r.clone())
            .unwrap_or_else(|| panic!("codegen: unknown ADT var {}", v)),
        HExpr::Ctor { adt, variant, args } => {
            let name = mdl.adt_name(*adt);
            // Alloca hoisted to the entry prelude: safe inside loops, and the
            // value is consumed within the same dynamic extent.
            let slot = c.temp();
            c.prelude
                .push(format!("  {} = alloca {}, align 8", slot, name));
            let t_tg = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 0",
                t_tg, name, slot
            ));
            c.line(&format!("  store i64 {}, ptr {}, align 8", variant, t_tg));
            let fields = mdl.adts[*adt as usize].variants[*variant as usize]
                .1
                .clone();
            for (k, (arg, fty)) in args.iter().zip(fields.iter()).enumerate() {
                let t_f = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                    t_f, name, slot, k
                ));
                match fty {
                    Ty::I64 => {
                        let v = emit_int(arg, c, mdl, sigs);
                        c.line(&format!("  store i64 {}, ptr {}, align 8", v, t_f));
                    }
                    Ty::F64 => {
                        let v = emit_float(arg, c, mdl, sigs);
                        let b = c.temp();
                        c.line(&format!("  {} = bitcast double {} to i64", b, v));
                        c.line(&format!("  store i64 {}, ptr {}, align 8", b, t_f));
                    }
                    Ty::Bool => {
                        let v = emit_bool(arg, c, mdl, sigs);
                        let z = c.temp();
                        c.line(&format!("  {} = zext i1 {} to i64", z, v));
                        c.line(&format!("  store i64 {}, ptr {}, align 8", z, t_f));
                    }
                    other => panic!("codegen: unsupported ADT field type {:?}", other),
                }
            }
            slot
        }
        // clone: materialize a fresh slot and copy the struct by value.
        HExpr::Clone(inner) => {
            let v = emit_adt(inner, c, mdl, sigs);
            let ai = adt_of(inner, c, sigs);
            let name = mdl.adt_name(ai);
            let slot = c.temp();
            c.prelude
                .push(format!("  {} = alloca {}, align 8", slot, name));
            let lv = c.temp();
            c.line(&format!("  {} = load {}, ptr {}, align 8", lv, name, v));
            c.line(&format!("  store {} {}, ptr {}, align 8", name, lv, slot));
            slot
        }
        HExpr::DropStmt(_) => panic!("codegen: drop produces no ADT value"),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Call(..) => call_user_node(e, c, mdl, sigs),
        other => panic!("codegen: expected an ADT expression, got {:?}", other),
    }
}

/// Emit a `match`: switch on the tag, load fields in each arm block, phi the
/// arm results in the merge block (scalars by value, ADTs as ptr).
fn emit_match_node(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::Match { scrut, arms, ty } = e else {
        panic!("codegen: emit_match_node on non-match")
    };
    let scrut_ptr = emit_adt(scrut, c, mdl, sigs);
    let ai = adt_of(scrut, c, sigs);
    let name = mdl.adt_name(ai);

    let t_tg = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 0",
        t_tg, name, scrut_ptr
    ));
    let t_tag = c.temp();
    c.line(&format!("  {} = load i64, ptr {}, align 8", t_tag, t_tg));

    let merged = c.block_name();
    let fail = c.block_name();
    let mut arm_labels: Vec<(Option<u32>, String)> = Vec::new();
    let mut default_lbl = fail.clone();
    for arm in arms {
        let lbl = c.block_name();
        if arm.variant.is_none() {
            // The wildcard arm is the switch default.
            default_lbl = lbl.clone();
        }
        arm_labels.push((arm.variant, lbl));
    }
    c.line(&format!("  switch i64 {}, label %{} [", t_tag, default_lbl));
    for (variant, lbl) in &arm_labels {
        if let Some(vi) = variant {
            c.line(&format!("    i64 {}, label %{}", vi, lbl));
        }
    }
    c.line("]");

    // Default: HIR guarantees exhaustiveness.
    let lbl = c.open(&fail);
    c.line(&format!("{}:", lbl));
    c.line("  unreachable");

    let mut vals: Vec<String> = Vec::new();
    let mut preds: Vec<String> = Vec::new();
    for (arm, (_, lbl)) in arms.iter().zip(arm_labels.iter()) {
        let l = c.open(lbl);
        c.line(&format!("{}:", l));
        if let Some(vi) = arm.variant {
            let fields = mdl.adts[ai as usize].variants[vi as usize]
                .1
                .clone();
            for (k, (binder, fty)) in arm.binders.iter().zip(fields.iter()).enumerate() {
                let t_f = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                    t_f, name, scrut_ptr, k
                ));
                match fty {
                    Ty::I64 => {
                        let t_l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                        c.scalar.insert(binder.clone(), (t_l, Ty::I64));
                    }
                    Ty::F64 => {
                        let t_l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                        let t_d = c.temp();
                        c.line(&format!("  {} = bitcast i64 {} to double", t_d, t_l));
                        c.scalar.insert(binder.clone(), (t_d, Ty::F64));
                    }
                    Ty::Bool => {
                        let t_l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                        let t_b = c.temp();
                        c.line(&format!("  {} = trunc i64 {} to i1", t_b, t_l));
                        c.scalar.insert(binder.clone(), (t_b, Ty::Bool));
                    }
                    other => panic!("codegen: unsupported ADT field type {:?}", other),
                }
            }
        }
        let v = emit_typed(&arm.body, *ty, c, mdl, sigs);
        let v = match ty {
            Ty::Adt(_) => v,
            t => pin_value(
                if *t == Ty::F64 {
                    "double"
                } else if *t == Ty::Bool {
                    "i1"
                } else {
                    "i64"
                },
                &v,
                c,
            ),
        };
        vals.push(v);
        preds.push(c.cur.clone());
        c.line(&format!("  br label %{}", merged));
    }

    let l = c.open(&merged);
    c.line(&format!("{}:", l));
    let res = c.temp();
    let lty = match ty {
        Ty::Adt(_) => "ptr".to_string(),
        Ty::Str => "ptr".to_string(),
        t => mdl.ll_ty(*t),
    };
    let pairs: Vec<String> = vals
        .iter()
        .zip(preds.iter())
        .map(|(v, p)| format!("[ {}, %{} ]", v, p))
        .collect();
    c.line(&format!("  {} = phi {} {}", res, lty, pairs.join(", ")));
    res
}

fn emit_int(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstI64(v) => v.to_string(),
        HExpr::Reduce(_) => panic!("codegen: Reduce in i64 context (rewrite bug)"),
        HExpr::Var(v) => {
            if let Some((r, _)) = c.scalar.get(v) {
                r.clone()
            } else {
                // vector var used in length context
                let (_, l, _) = c.vecs.get(v).expect("known var");
                l.clone()
            }
        }
        HExpr::Bin(op, a, b) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let op = match op {
                BinOp::Add => "add",
                BinOp::Sub => "sub",
                BinOp::Mul => "mul",
                BinOp::Div => "sdiv",
            };
            let t = c.temp();
            c.line(&format!("  {} = {} i64 {}, {}", t, op, av, bv));
            t
        }
        HExpr::ConvF64(_) => panic!("codegen: ConvF64 in i64 context"),
        HExpr::Call(name, args) if name == "print-i64" => {
            // Side-effecting call inside a loop body (e.g. doseq).
            let v = emit_int(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_i64(i64 {})", v));
            "0".to_string()
        }
        HExpr::Call(name, args) if name == "print-string" => {
            let v = emit_str_val(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_string(ptr {})", v));
            "0".to_string()
        }
        HExpr::Count(v) => match &**v {
            HExpr::Var(s) => {
                if let Some((_, l, _)) = c.vecs.get(s) {
                    l.clone()
                } else {
                    let (r, ty) = c
                        .scalar
                        .get(s)
                        .cloned()
                        .unwrap_or_else(|| panic!("codegen: count of unknown var {}", s));
                    if ty == Ty::Str {
                        mdl.declare("declare i64 @strlen(ptr)".to_string());
                        let t = c.temp();
                        c.line(&format!("  {} = call i64 @strlen(ptr {})", t, r));
                        t
                    } else {
                        panic!("codegen: count of non-vector/non-string {}", s)
                    }
                }
            }
            HExpr::StrLit(_) => {
                let p = emit_str_val(v, c, mdl, sigs);
                mdl.declare("declare i64 @strlen(ptr)".to_string());
                let t = c.temp();
                c.line(&format!("  {} = call i64 @strlen(ptr {})", t, p));
                t
            }
            other => panic!("codegen: count of {:?}", other),
        },
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
        HExpr::Let(binds, body) => {
            emit_let(binds, c, mdl, sigs);
            emit_int(body, c, mdl, sigs)
        }
        HExpr::Seq(vs) => {
            for v in vs {
                emit_stmt(v, c, mdl, sigs);
            }
            "0".to_string()
        }
        HExpr::If(cond, then, els, t) => emit_if_node(cond, then, els, *t, c, mdl, sigs),
        HExpr::Abs(e, _) => {
            let v = emit_int(e, c, mdl, sigs);
            let neg = c.temp();
            c.line(&format!("  {} = sub i64 0, {}", neg, v));
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, 0", lt, v));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, neg, v));
            t
        }
        HExpr::Min(a, b, _) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, av, bv));
            t
        }
        HExpr::Max(a, b, _) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = icmp sgt i64 {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, av, bv));
            t
        }
        HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Cmp(..) => {
            panic!("codegen: bool expression in i64 context")
        }
        HExpr::ConstF64(v) => panic!("codegen: ConstF64 {} in i64 context", v),
        HExpr::VecLit(_) => panic!("codegen: vec literal in i64 context"),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Ctor { .. } => {
            emit_adt(e, c, mdl, sigs);
            "0".to_string()
        }
        HExpr::Get { ty: Ty::I64, .. } => emit_get(e, c, mdl, sigs),
        HExpr::Clone(inner) => emit_int(inner, c, mdl, sigs),
        HExpr::DropStmt(_) => "0".to_string(),
        HExpr::StrLit(_) => panic!("codegen: string literal in i64 context"),
        HExpr::StrCmp(..) => panic!("codegen: string comparison in i64 context"),
        HExpr::Get { ty, .. } => panic!(
            "codegen: get returning {:?} in i64 context (expected F64 context)",
            ty
        ),
    }
    .to_string()
}

/// Emit an expression of string type: a `ptr` to the literal's first byte
/// (or a string value held in a variable).
fn emit_str_val(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    _sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::StrLit(s) => {
            let label = mdl.mint_str(s);
            let n = s.bytes().count() + 1;
            let p = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds [{} x i8], ptr {}, i32 0, i32 0",
                p, n, label
            ));
            p
        }
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(r, _)| r.clone())
            .unwrap_or_else(|| panic!("codegen: unknown string var {}", v)),
        other => panic!("codegen: expected a string expression, got {:?}", other),
    }
}

/// `(get vec idx [default])` — branchless bounds-checked element load. The
/// result has the vector's element type, so this is valid in int and float
/// contexts alike.
fn emit_get(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::Get {
        src,
        idx,
        default,
        ty,
    } = e
    else {
        panic!("codegen: emit_get on non-get")
    };
    let (d, l, elem) = vec_arg(src, c, mdl);
    let ll = mdl.elem_ll(elem);
    let iv = emit_int(idx, c, mdl, sigs);
    let inb = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, l));
    let safe = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", safe, inb, iv));
    let p = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        p, ll, d, safe
    ));
    let v = c.temp();
    c.line(&format!("  {} = load {}, ptr {}, align 8", v, ll, p));
    match default {
        Some(dv) => {
            let dvv = emit_typed(dv, *ty, c, mdl, sigs);
            let r = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, {} {}, {} {}",
                r,
                inb,
                ll,
                v,
                ll,
                dvv
            ));
            r
        }
        None if *ty == Ty::I64 => {
            let r = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", r, inb, v));
            r
        }
        None => v,
    }
}

fn emit_float(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstF64(v) => llvm_f64(*v),
        HExpr::Get { ty: Ty::F64, .. } => emit_get(e, c, mdl, sigs),
        HExpr::Clone(inner) => emit_float(inner, c, mdl, sigs),
        HExpr::Var(v) => {
            let (r, _) = c.scalar.get(v).expect("known f64 var");
            r.clone()
        }
        HExpr::Bin(op, a, b) => {
            let av = emit_float(a, c, mdl, sigs);
            let bv = emit_float(b, c, mdl, sigs);
            let op = match op {
                BinOp::Add => "fadd",
                BinOp::Sub => "fsub",
                BinOp::Mul => "fmul",
                BinOp::Div => "fdiv",
            };
            let t = c.temp();
            c.line(&format!("  {} = {} double {}, {}", t, op, av, bv));
            t
        }
        HExpr::ConvF64(v) => {
            let iv = emit_int(v, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = sitofp i64 {} to double", t, iv));
            t
        }
        HExpr::Call(name, args) => {
            if name == "print-f64" {
                let v = emit_float(&args[0], c, mdl, sigs);
                c.line(&format!("  call void @cato_print_f64(double {})", v));
                "0.0".to_string()
            } else {
                call_user(name, args, c, mdl, sigs)
            }
        }
        HExpr::Let(binds, body) => {
            emit_let(binds, c, mdl, sigs);
            emit_float(body, c, mdl, sigs)
        }
        HExpr::Seq(vs) => {
            for v in vs {
                emit_stmt(v, c, mdl, sigs);
            }
            "0.0".to_string()
        }
        HExpr::ConstI64(v) => {
            let t = c.temp();
            c.line(&format!("  {} = sitofp i64 {} to double", t, v));
            t
        }
        HExpr::If(cond, then, els, t) => emit_if_node(cond, then, els, *t, c, mdl, sigs),
        HExpr::Abs(e, _) => {
            let v = emit_float(e, c, mdl, sigs);
            let neg = c.temp();
            c.line(&format!("  {} = fneg double {}", neg, v));
            let lt = c.temp();
            c.line(&format!("  {} = fcmp olt double {}, 0.000000e+00", lt, v));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, neg, v));
            t
        }
        HExpr::Min(a, b, _) => {
            let av = emit_float(a, c, mdl, sigs);
            let bv = emit_float(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = fcmp olt double {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, av, bv));
            t
        }
        HExpr::Max(a, b, _) => {
            let av = emit_float(a, c, mdl, sigs);
            let bv = emit_float(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = fcmp ogt double {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, av, bv));
            t
        }
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Ctor { .. } => {
            emit_adt(e, c, mdl, sigs);
            "0.0".to_string()
        }
        other => panic!("codegen: unsupported node in f64 context: {:?}", other),
    }
}

fn emit_let(binds: &[(String, HExpr, Ty)], c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
    for (name, value, ty) in binds {
        match ty {
            Ty::F64 => {
                let v = emit_float(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::F64));
            }
            Ty::Bool => {
                let v = emit_bool(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::Bool));
            }
            Ty::Vec(_) => materialize_vec_lit(value, name, c, mdl),
            Ty::Adt(_) => {
                let v = emit_adt(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, *ty));
            }
            _ => {
                let v = emit_int(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::I64));
            }
        }
    }
}

fn materialize_vec_lit(value: &HExpr, name: &str, c: &mut FnCtx, mdl: &mut Mdl) {
    let HExpr::VecLit(lit) = value else {
        panic!("codegen: can only materialize vector literal bindings")
    };
    let (label, len, elem) = mdl.mint_vec(lit);
    let t_v = c.temp();
    c.line(&format!("  {} = alloca %VecI64, align 8", t_v));
    c.line(&format!(
        "  store %VecI64 {{ ptr {}, i64 {}, i64 {} }}, ptr {}, align 8",
        label, len, len, t_v
    ));
    let t_lv = c.temp();
    c.line(&format!("  {} = load %VecI64, ptr {}, align 8", t_lv, t_v));
    let t_d = c.temp();
    let t_l = c.temp();
    c.line(&format!("  {} = extractvalue %VecI64 {}, 0", t_d, t_lv));
    c.line(&format!("  {} = extractvalue %VecI64 {}, 1", t_l, t_lv));
    c.vecs.insert(name.to_string(), (t_d, t_l, elem));
}

fn vec_arg(expr: &HExpr, c: &mut FnCtx, mdl: &mut Mdl) -> (String, String, Elem) {
    match expr {
        HExpr::Var(v) => c
            .vecs
            .get(v)
            .cloned()
            .unwrap_or_else(|| panic!("codegen: {} is not a vector binding", v)),
        HExpr::VecLit(lit) => {
            let (label, len, elem) = mdl.mint_vec(lit);
            let t_v = c.temp();
            c.line(&format!("  {} = alloca %VecI64, align 8", t_v));
            c.line(&format!(
                "  store %VecI64 {{ ptr {}, i64 {}, i64 {} }}, ptr {}, align 8",
                label, len, len, t_v
            ));
            let t_lv = c.temp();
            c.line(&format!("  {} = load %VecI64, ptr {}, align 8", t_lv, t_v));
            let t_d = c.temp();
            let t_l = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 0", t_d, t_lv));
            c.line(&format!("  {} = extractvalue %VecI64 {}, 1", t_l, t_lv));
            (t_d, t_l, elem)
        }
        _ => panic!("codegen: unsupported vector argument"),
    }
}

fn call_user_node(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
        other => panic!("codegen: expected a call, got {:?}", other),
    }
}

fn call_user(
    name: &str,
    args: &[HExpr],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {    let (ret_ty, arg_tys) = sigs.get(name).unwrap_or_else(|| panic!("codegen: unknown call {}", name));
    let arg_tys = arg_tys.clone();
    let mut ll_args = Vec::new();
    for (expr, ty) in args.iter().zip(arg_tys.iter()) {
        match ty {
            Ty::Vec(_) => {
                let (d, l, _) = vec_arg(expr, c, mdl);
                ll_args.push(format!("ptr {}", d));
                ll_args.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let v = emit_float(expr, c, mdl, sigs);
                ll_args.push(format!("double {}", v));
            }
            Ty::I64 => {
                let v = emit_int(expr, c, mdl, sigs);
                ll_args.push(format!("i64 {}", v));
            }
            Ty::Bool => {
                let v = emit_bool(expr, c, mdl, sigs);
                ll_args.push(format!("i1 {}", v));
            }
            Ty::Param(_) => panic!("codegen: unresolved type parameter in call args"),
            Ty::Str => {
                let v = emit_str_val(expr, c, mdl, sigs);
                ll_args.push(format!("ptr {}", v));
            }
            Ty::Adt(i) => {
                // By-value boundary: load the caller's slot into the arg.
                let v = emit_adt(expr, c, mdl, sigs);
                let lv = c.temp();
                c.line(&format!(
                    "  {} = load {}, ptr {}, align 8",
                    lv,
                    mdl.adt_name(*i),
                    v
                ));
                ll_args.push(format!("{} {}", mdl.adt_name(*i), lv));
            }
        }
    }
    let adt_ret = matches!(ret_ty, Ty::Adt(_));
    let ret_ll = if *ret_ty == Ty::Bool {
        "i1".to_string()
    } else if *ret_ty == Ty::F64 {
        "double".to_string()
    } else if let Ty::Adt(i) = *ret_ty {
        mdl.adt_name(i)
    } else if *ret_ty == Ty::Str {
        "ptr".to_string()
    } else {
        "i64".to_string()
    };
    let t = c.temp();
    c.line(&format!(
        "  {} = call {} @{}({})",
        t,
        ret_ll,
        mangle(name),
        ll_args.join(", ")
    ));
    if !adt_ret {
        return t;
    }
    // ADT return: store the by-value struct into a fresh caller slot and
    // hand out the ptr (internal representation stays ptr-based).
    let i = match ret_ty {
        Ty::Adt(i) => *i,
        _ => unreachable!(),
    };
    let slot = c.temp();
    c.prelude
        .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(i)));
    c.line(&format!(
        "  store {} {}, ptr {}, align 8",
        mdl.adt_name(i),
        t,
        slot
    ));
    slot
}

// ---------------------------------------------------------------------------
// Free variable collection
// ---------------------------------------------------------------------------

#[derive(Default)]
struct VarSet {
    vars: Vec<String>,
}

fn walk_source<F: FnMut(String)>(src: &HSource, f: &mut F) {
    match src {
        HSource::Var(v) => f(v.clone()),
        HSource::Lit(_) => {}
        HSource::Range(a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HSource::Zip(a, b) => {
            walk_source(a, f);
            walk_source(b, f);
        }
        HSource::Take(n, inner) | HSource::Drop(n, inner) => {
            collect_vars(n, f);
            walk_source(inner, f);
        }
    }
}

impl VarSet {
    fn new() -> Self {
        Self { vars: Vec::new() }
    }
    fn remove(&mut self, v: &str) {
        self.vars.retain(|x| x != v);
    }
    fn insert_expr(&mut self, e: &HExpr) {
        collect_vars(e, &mut |v: String| {
            if !self.vars.contains(&v) {
                self.vars.push(v);
            }
        });
    }
}

fn collect_vars<F: FnMut(String)>(h: &HExpr, f: &mut F) {
    match h {
        HExpr::Var(v) => f(v.clone()),
        HExpr::Bin(_, a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::ConvF64(e) => collect_vars(e, f),
        HExpr::Count(e) => collect_vars(e, f),
        HExpr::If(c, t, e, _) => {
            collect_vars(c, f);
            collect_vars(t, f);
            collect_vars(e, f);
        }
        HExpr::And(a, b) | HExpr::Or(a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Not(v) => collect_vars(v, f),
        HExpr::Cmp(_, a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Abs(v, _) => collect_vars(v, f),
        HExpr::Clone(v) | HExpr::DropStmt(v) => collect_vars(v, f),
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline {
                init, maps, filters, src, ..
            } => {
                collect_vars(init, f);
                walk_source(src, f);
                for m in maps {
                    collect_vars(&m.body, f);
                }
                for fl in filters {
                    collect_vars(&fl.body, f);
                }
            }
            HReduce::Closure {
                init,
                src,
                body,
                maps,
                filters,
                ..
            } => {
                collect_vars(init, f);
                walk_source(src, f);
                for m in maps {
                    collect_vars(&m.body, f);
                }
                for fl in filters {
                    collect_vars(&fl.body, f);
                }
                collect_vars(body, f);
            }
        },
        HExpr::Get { idx, default, .. } => {
            collect_vars(idx, f);
            if let Some(d) = default {
                collect_vars(d, f);
            }
        }
        HExpr::Call(_, args) => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::Let(binds, body) => {
            for (_, e, _) in binds {
                collect_vars(e, f);
            }
            collect_vars(body, f);
        }
        HExpr::Seq(vs) => {
            for v in vs {
                collect_vars(v, f);
            }
        }
        HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::VecLit(_) | HExpr::StrLit(_) => {}
        HExpr::StrCmp(_, a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Ctor { args, .. } => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::Match { scrut, arms, .. } => {
            collect_vars(scrut, f);
            // Arm binders are re-bound inside the helper's arm blocks, so
            // only the scrutinee and the bodies contribute captures.
            for arm in arms {
                collect_vars(&arm.body, f);
            }
        }
    }
}