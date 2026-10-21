//! Dead-code elimination and copy propagation on `Let` bindings.
//!
//! Both transforms only ever touch bindings whose init is droppable-pure,
//! and copy propagation only substitutes inits that cannot capture
//! (closed expressions or a lone `Var`). Stale free-list entries left by
//! removed bindings are rebuilt by the driver's `recompute_frees`.

use super::walk::{count_uses, is_drop_safe, rewrite, substitute};
use crate::hir::*;
use crate::Program;
use std::cell::Cell;

pub fn run(program: &mut Program) -> bool {
    let mut changed = false;
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        changed |= simplify_body(&mut d.body);
    }
    for lam in program.lambdas.iter_mut() {
        changed |= simplify_body(&mut lam.body);
    }
    changed
}

fn simplify_body(body: &mut HExpr) -> bool {
    let changed = Cell::new(false);
    let taken = std::mem::replace(body, HExpr::ConstI64(0));
    *body = rewrite(taken, &mut |e| simplify_one(e, &changed));
    changed.get()
}

fn simplify_one(e: HExpr, changed: &Cell<bool>) -> HExpr {
    match e {
        HExpr::Let(binds, body, frees, _) => simplify_let(binds, *body, frees, changed),
        HExpr::Seq(vs) => {
            if vs.len() <= 1 {
                return HExpr::Seq(vs);
            }
            let last = vs.last().unwrap().clone();
            let kept: Vec<HExpr> = vs[..vs.len() - 1]
                .iter()
                .filter(|v| !is_drop_safe(v))
                .cloned()
                .collect();
            let dropped = vs.len() - 1 - kept.len();
            if dropped == 0 {
                return HExpr::Seq(vs);
            }
            changed.set(true);
            if kept.is_empty() {
                last
            } else {
                let mut kept = kept;
                kept.push(last);
                HExpr::Seq(kept)
            }
        }
        other => other,
    }
}

fn simplify_let(
    binds: Vec<(String, HExpr, Ty)>,
    body: HExpr,
    frees: Vec<String>,
    changed: &Cell<bool>,
) -> HExpr {
    let n = binds.len();
    // Decisions are made against the original tree: substituting binder i
    // can only change how often binders < i are used (their inits may
    // mention i's name? no — inits only see earlier binders), so forward
    // pass counts stay valid.
    enum Decision {
        Keep,
        Drop,
        Copy(HExpr),
    }
    let mut decisions: Vec<Decision> = Vec::with_capacity(n);
    for (i, (name, init, ty)) in binds.iter().enumerate() {
        if !is_drop_safe(init) || !copyable(init) {
            decisions.push(Decision::Keep);
            continue;
        }
        // Heap-owning bindings must stay bound: substituting an allocation
        // into a non-binding position would create an unowned intermediate
        // that the scope-exit free lists can never release. Owning ADTs,
        // strings and closures bind heap the same way collections do
        // (heap_owning), so they stay bound too. Dropping an unused one is
        // still fine below — it simply never allocates.
        if matches!(
            ty,
            Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Str | Ty::Fn(_) | Ty::Adt(_)
        ) {
            decisions.push(Decision::Keep);
            continue;
        }
        let mut refs: Vec<&HExpr> = binds[i + 1..].iter().map(|(_, e, _)| e).collect();
        refs.push(&body);
        let counts = count_uses(std::slice::from_ref(name), &refs);
        match counts.get(name).copied().unwrap_or(0) {
            0 => decisions.push(Decision::Drop),
            1 => decisions.push(Decision::Copy(init.clone())),
            _ => decisions.push(Decision::Keep),
        }
    }

    // Apply: substitutions of earlier binders flow into later inits and the
    // body; dropped binders vanish.
    let mut new_binds: Vec<(String, HExpr, Ty)> = Vec::with_capacity(n);
    let mut pending: Vec<(String, HExpr)> = Vec::new();
    let mut any = false;
    for ((name, init, ty), d) in binds.into_iter().zip(decisions) {
        let mut init = init;
        for (pname, prepl) in &pending {
            let taken = std::mem::replace(&mut init, HExpr::ConstI64(0));
            init = substitute(taken, pname, prepl);
        }
        match d {
            Decision::Keep => new_binds.push((name, init, ty)),
            Decision::Drop => any = true,
            Decision::Copy(repl) => {
                any = true;
                pending.push((name, repl));
            }
        }
    }
    let mut body = body;
    for (pname, prepl) in &pending {
        let taken = std::mem::replace(&mut body, HExpr::ConstI64(0));
        body = substitute(taken, pname, prepl);
    }

    if !any {
        return HExpr::Let(new_binds, Box::new(body), frees, Vec::new());
    }
    changed.set(true);
    if new_binds.is_empty() {
        body
    } else {
        HExpr::Let(new_binds, Box::new(body), frees, Vec::new())
    }
}

/// May this init be substituted at its (unique) use site without capture
/// risk? Closed expressions reference nothing; a lone `Var` references
/// exactly the name being substituted for, which the scoped substitution
/// already handles.
fn copyable(init: &HExpr) -> bool {
    if !super::walk::has_var_refs(init) {
        return true;
    }
    matches!(init, HExpr::Var(_))
}
