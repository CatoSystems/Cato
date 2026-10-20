use super::*;

pub(crate) struct LoopSpec {
    pub(crate) helper: String,
    pub(crate) acc_ty: Ty, // I64 or F64, from the init literal
    pub(crate) caps: Vec<(String, Ty)>,
    /// Element type of each base vector, in call-site argument order.
    pub(crate) base_elems: Vec<Elem>,
    pub(crate) reduce: HReduce,
    /// Parallel chunked emission requested by `(parallel ...)` and still
    /// qualified (vector source, scalar captures).
    pub(crate) parallel: bool,
}

/// Parallel eligibility at the ABI level: every capture must be a scalar
/// (the runner stores them into a plain arg block).
fn caps_parallel_ok(caps: &[(String, Ty)]) -> bool {
    caps.iter().all(|(_, t)| t.is_int() || matches!(t, Ty::F64 | Ty::F32 | Ty::Bool | Ty::Str))
}

/// Element type of each base vector of a loop source (argument order).
pub(crate) fn source_base_elems(src: &HSource, params: &[(String, Ty)]) -> Vec<Elem> {
    match src {
        // The innermost binding of the name wins; an unknown or non-vector
        // source is a compiler bug — never a guessed element width.
        HSource::Var(v) => vec![match params.iter().rev().find(|(n, _)| n == v) {
            Some((_, Ty::Vec(e))) => *e,
            other => panic!("codegen: loop source {} has no vector type in scope ({:?})", v, other),
        }],
        HSource::Lit(vl) => vec![vl.elem()],
        HSource::Zip(a, b) => {
            let mut out = source_base_elems(a, params);
            out.extend(source_base_elems(b, params));
            out
        }
        HSource::Range(..) => Vec::new(),
        HSource::Take(_, inner) | HSource::Drop(_, inner) => source_base_elems(inner, params),
    }
}

pub(crate) fn make_loop_spec(helper: &str, reduce: HReduce, params: &[(String, Ty)]) -> LoopSpec {
    let (acc_ty, mut free) = loop_free_vars(&reduce);
    let mut caps: Vec<(String, Ty)> = free
        .vars
        .drain(..)
        .map(|v| {
            let t = params
                .iter()
                .rev()
                .find(|(n, _)| *n == v)
                .map(|(_, t)| *t)
                .unwrap_or_else(|| panic!("codegen: reduce capture {} has no type in scope", v));
            (v, t)
        })
        .collect();
    caps.sort_by(|a, b| a.0.cmp(&b.0));
    let base_elems = source_base_elems(reduce_src(&reduce), params);
    let requested = match &reduce {
        HReduce::Pipeline { parallel, src, .. } => {
            *parallel && matches!(src, HSource::Var(_))
        }
        _ => false,
    };
    let parallel = requested && caps_parallel_ok(&caps);
    LoopSpec {
        helper: helper.to_string(),
        acc_ty,
        caps,
        base_elems,
        reduce,
        parallel,
    }
}

/// Free variables contributed by a loop source: vector-variable names for
/// `Var`/`Zip` bases (passed as data/len args, not captures) are excluded,
/// while `range`/`take`/`drop` bound expressions are ordinary captures.
pub(crate) fn collect_source_vars(src: &HSource, bv: &mut VarSet, skip: &mut Vec<String>) {
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
pub(crate) fn source_base_args(src: &HSource) -> Vec<HExpr> {
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
pub(crate) fn base_vec_vars(src: &HSource, out: &mut Vec<String>) {
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

pub(crate) fn loop_free_vars(reduce: &HReduce) -> (Ty, VarSet) {
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
    let t = match reduce {
        HReduce::Pipeline { acc_ty, .. } | HReduce::Closure { acc_ty, .. } => *acc_ty,
    };
    (t, bv)
}

pub(crate) fn reduce_init(r: &HReduce) -> Option<&HExpr> {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => Some(init),
    }
}

/// An f64 accumulator: an f64 init literal or a promoted int init
/// (an `ICast` to f64 inserted by HIR when elements are f64).
pub(crate) fn acc_ret_is_f64(r: &HReduce) -> bool {
    match r {
        HReduce::Pipeline { acc_ty, .. } | HReduce::Closure { acc_ty, .. } => *acc_ty == Ty::F64,
    }
}

pub(crate) fn init_is_f64(init: &HExpr) -> bool {
    match init {
        HExpr::ConstF64(_) => true,
        HExpr::ICast(_, _, Ty::F64) => true,
        _ => false,
    }
}

/// Replace every `reduce` node with a call to its hoisted helper, collecting
/// the helpers. Returns the rewritten expression.
pub(crate) fn reduce_src(r: &HReduce) -> &HSource {
    match r {
        HReduce::Pipeline { src, .. } | HReduce::Closure { src, .. } => src,
    }
}

pub(crate) struct VarSet {
    pub(crate) vars: Vec<String>,
}

pub(crate) fn walk_source<F: FnMut(String)>(src: &HSource, f: &mut F) {
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
    pub(crate) fn new() -> Self {
        Self { vars: Vec::new() }
    }
    pub(crate) fn remove(&mut self, v: &str) {
        self.vars.retain(|x| x != v);
    }
    pub(crate) fn insert_expr(&mut self, e: &HExpr) {
        collect_vars(e, &mut |v: String| {
            if !self.vars.contains(&v) {
                self.vars.push(v);
            }
        });
    }
}


pub(crate) fn unwrap_source(src: &HSource) -> (Vec<(&HExpr, bool)>, &HSource) {
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
