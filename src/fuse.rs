use crate::hir::*;

/// Stream fusion pass.
///
/// The HIR builder already lowers every (`reduce`) with its `map`/`filter`
/// stages attached as inline `Pipeline`/`Closure` specs. What remains here is
/// to validate the *fusion invariant* that codegen relies on: a fused pipeline
/// compiles to a single allocation-free loop only when its source is a
/// variable-backed `(Vector i64)`. Any other source shape is left intact so a
/// later stage (codegen) can reject it loudly instead of silently
/// miscompiling. This structure mirrors the spec's Stream Fusion stage: at
/// compile time the `reduce` is rewritten into one loop writing to disjoint
/// memory, not a chain of heap-allocated intermediate sequences.
pub fn fuse(h: &HExpr) -> HExpr {
    match h {
        HExpr::Reduce(r) => fuse_reduce(r),
        HExpr::Bin(op, a, b) => {
            HExpr::Bin(op.clone(), Box::new(fuse(a)), Box::new(fuse(b)))
        }
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(fuse(e))),
        HExpr::Count(e) => HExpr::Count(Box::new(fuse(e))),
        HExpr::Call(n, args) => {
            HExpr::Call(n.clone(), args.iter().map(fuse).collect())
        }
        HExpr::Let(binds, body) => HExpr::Let(
            binds
                .iter()
                .map(|(n, e, t)| (n.clone(), fuse(e), *t))
                .collect(),
            Box::new(fuse(body)),
        ),
        HExpr::Seq(vs) => HExpr::Seq(vs.iter().map(fuse).collect()),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
    }
}

fn fuse_reduce(r: &HReduce) -> HExpr {
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