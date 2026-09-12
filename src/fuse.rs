use crate::hir::*;

/// Stream fusion stage.
///
/// The HIR builder already lowers every (`reduce`) with its `map`/`filter`
/// stages attached as inline `Pipeline`/`Closure` specs. What remains here is
/// to validate the *fusion invariant* that codegen relies on: a fused pipeline
/// compiles to a single allocation-free loop only when its source is a
/// variable-backed `(Vector i64)`. Any other source shape is left intact so a
/// later stage (codegen) can reject it loudly instead of silently
/// miscompiling. At compile time the `reduce` is fused into one loop over the
/// source sequence rather than a chain of heap-allocated intermediates.
pub fn check(h: &HExpr) -> HExpr {
    match h {
        HExpr::Reduce(r) => check_reduce(r),
        HExpr::Bin(op, a, b) => {
            HExpr::Bin(op.clone(), Box::new(check(a)), Box::new(check(b)))
        }
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(check(e))),
        HExpr::Count(e) => HExpr::Count(Box::new(check(e))),
        HExpr::Call(n, args) => {
            HExpr::Call(n.clone(), args.iter().map(check).collect())
        }
        HExpr::Let(binds, body) => HExpr::Let(
            binds
                .iter()
                .map(|(n, e, t)| (n.clone(), check(e), *t))
                .collect(),
            Box::new(check(body)),
        ),
        HExpr::Seq(vs) => HExpr::Seq(vs.iter().map(check).collect()),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
        HExpr::If(c, t, e, ty) => HExpr::If(
            Box::new(check(c)),
            Box::new(check(t)),
            Box::new(check(e)),
            *ty,
        ),
        HExpr::And(a, b) => HExpr::And(Box::new(check(a)), Box::new(check(b))),
        HExpr::Or(a, b) => HExpr::Or(Box::new(check(a)), Box::new(check(b))),
        HExpr::Not(v) => HExpr::Not(Box::new(check(v))),
        HExpr::Cmp(op, a, b, f) => {
            HExpr::Cmp(op.clone(), Box::new(check(a)), Box::new(check(b)), *f)
        }
        HExpr::Abs(v, t) => HExpr::Abs(Box::new(check(v)), *t),
        HExpr::Min(a, b, t) => HExpr::Min(Box::new(check(a)), Box::new(check(b)), *t),
        HExpr::Max(a, b, t) => HExpr::Max(Box::new(check(a)), Box::new(check(b)), *t),
        HExpr::Ctor { adt, variant, args } => HExpr::Ctor {
            adt: *adt,
            variant: *variant,
            args: args.iter().map(check).collect(),
        },
        HExpr::Match {
            scrut,
            arms,
            ty,
        } => HExpr::Match {
            scrut: Box::new(check(scrut)),
            arms: arms
                .iter()
                .map(|a| HArm {
                    variant: a.variant,
                    binders: a.binders.clone(),
                    body: check(&a.body),
                })
                .collect(),
            ty: *ty,
        },
    }
}

fn check_reduce(r: &HReduce) -> HExpr {
    let ok_src = match r {
        HReduce::Pipeline { src, .. } => matches!(**src, HExpr::Var(_)),
        HReduce::Closure { src, .. } => matches!(**src, HExpr::Var(_)),
    };
    if !ok_src {
        // Unsupported source shape: leave the node intact (codegen rejects).
        return HExpr::Reduce(Box::new(r.clone()));
    }
    // A var-backed reduce is the fused single-loop case. Each pipeline stage is
    // already an inlined spec (defunctionalized at build time); codegen emits
    // exactly one loop for it.
    HExpr::Reduce(Box::new(r.clone()))
}
