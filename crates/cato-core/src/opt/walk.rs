//! HIR traversal and analysis utilities shared by every optimization pass.
//!
//! Historically each tree walk in the project hand-rolled a ~45-arm
//! recursion (see `fuse::check` and the backend's `rewrite_body`); the
//! passes here build on the shared `rewrite` / scoped-counting helpers
//! instead.

use crate::hir::*;
use std::collections::{HashMap, HashSet};

/// Bottom-up rewrite: maps `f` over every node of `e`, children first.
/// `f` sees each node after its children have been rewritten and may return
/// the node unchanged. The `Reduce` fused-loop node is traversed into (its
/// stage bodies, init and source bounds are ordinary expressions) but its
/// own shape is never restructured — the single-fused-loop contract that
/// codegen relies on stays intact.
pub fn rewrite(e: HExpr, f: &mut dyn FnMut(HExpr) -> HExpr) -> HExpr {
    let e = match e {
        HExpr::ConstI64(_) | HExpr::ConstB(_) | HExpr::ConstF64(_) | HExpr::Var(_)
        | HExpr::StrLit(_) | HExpr::VecLit(_) => return f(e),
        HExpr::Bin(op, a, b) => {
            HExpr::Bin(op, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)))
        }
        HExpr::ConvF64(a) => HExpr::ConvF64(Box::new(rewrite(*a, f))),
        HExpr::Count(a) => HExpr::Count(Box::new(rewrite(*a, f))),
        HExpr::Call(n, args) => {
            HExpr::Call(n, args.into_iter().map(|a| rewrite(a, f)).collect())
        }
        HExpr::Let(binds, body, frees) => HExpr::Let(
            binds
                .into_iter()
                .map(|(n, e, t)| (n, rewrite(e, f), t))
                .collect(),
            Box::new(rewrite(*body, f)),
            frees,
        ),
        HExpr::Seq(vs) => HExpr::Seq(vs.into_iter().map(|v| rewrite(v, f)).collect()),
        HExpr::If(c, t, e2, ty) => HExpr::If(
            Box::new(rewrite(*c, f)),
            Box::new(rewrite(*t, f)),
            Box::new(rewrite(*e2, f)),
            ty,
        ),
        HExpr::And(a, b) => {
            HExpr::And(Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)))
        }
        HExpr::Or(a, b) => HExpr::Or(Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f))),
        HExpr::Not(a) => HExpr::Not(Box::new(rewrite(*a, f))),
        HExpr::Cmp(op, a, b, fl) => {
            HExpr::Cmp(op, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)), fl)
        }
        HExpr::BinW(op, a, b, t) => {
            HExpr::BinW(op, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)), t)
        }
        HExpr::CmpW(op, a, b, t) => {
            HExpr::CmpW(op, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)), t)
        }
        HExpr::BinF(op, a, b) => {
            HExpr::BinF(op, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)))
        }
        HExpr::ICast(a, t) => HExpr::ICast(Box::new(rewrite(*a, f)), t),
        HExpr::Abs(a, t) => HExpr::Abs(Box::new(rewrite(*a, f)), t),
        HExpr::Min(a, b, t) => {
            HExpr::Min(Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)), t)
        }
        HExpr::Max(a, b, t) => {
            HExpr::Max(Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)), t)
        }
        HExpr::StrCat(args) => {
            HExpr::StrCat(args.into_iter().map(|a| rewrite(a, f)).collect())
        }
        HExpr::StrCmp(p, a, b) => {
            HExpr::StrCmp(p, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)))
        }
        HExpr::StrOrd(op, a, b) => {
            HExpr::StrOrd(op, Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)))
        }
        HExpr::StrNth { s, idx } => HExpr::StrNth {
            s: Box::new(rewrite(*s, f)),
            idx: Box::new(rewrite(*idx, f)),
        },
        HExpr::CharStr(c) => HExpr::CharStr(Box::new(rewrite(*c, f))),
        HExpr::Loop { binds, body, ty } => HExpr::Loop {
            binds: binds
                .into_iter()
                .map(|(n, t, e)| (n, t, rewrite(e, f)))
                .collect(),
            body: Box::new(rewrite(*body, f)),
            ty,
        },
        HExpr::Recur(args) => HExpr::Recur(args.into_iter().map(|a| rewrite(a, f)).collect()),
        HExpr::ToStr(e, t) => HExpr::ToStr(Box::new(rewrite(*e, f)), t),
        HExpr::ParseNum { s, int, res } => HExpr::ParseNum {
            s: Box::new(rewrite(*s, f)),
            int,
            res,
        },
        HExpr::Slurp(p) => HExpr::Slurp(Box::new(rewrite(*p, f))),
        HExpr::Spit { path, data } => HExpr::Spit {
            path: Box::new(rewrite(*path, f)),
            data: Box::new(rewrite(*data, f)),
        },
        HExpr::Args => HExpr::Args,
        HExpr::Exit(c) => HExpr::Exit(Box::new(rewrite(*c, f))),
        HExpr::MapLit(pairs) => HExpr::MapLit(
            pairs
                .into_iter()
                .map(|(k, v)| (rewrite(k, f), rewrite(v, f)))
                .collect(),
        ),
        HExpr::MapAssoc { m, k, v } => HExpr::MapAssoc {
            m: Box::new(rewrite(*m, f)),
            k: Box::new(rewrite(*k, f)),
            v: Box::new(rewrite(*v, f)),
        },
        HExpr::MapGet { src, key, default } => HExpr::MapGet {
            src: Box::new(rewrite(*src, f)),
            key: Box::new(rewrite(*key, f)),
            default: default.map(|d| Box::new(rewrite(*d, f))),
        },
        HExpr::SetLit(vs) => HExpr::SetLit(vs.into_iter().map(|v| rewrite(v, f)).collect()),
        HExpr::SetConj { s, x } => HExpr::SetConj {
            s: Box::new(rewrite(*s, f)),
            x: Box::new(rewrite(*x, f)),
        },
        HExpr::SetDisj { s, x } => HExpr::SetDisj {
            s: Box::new(rewrite(*s, f)),
            x: Box::new(rewrite(*x, f)),
        },
        HExpr::SetContains { s, x } => HExpr::SetContains {
            s: Box::new(rewrite(*s, f)),
            x: Box::new(rewrite(*x, f)),
        },
        HExpr::Subs { s, start, end } => HExpr::Subs {
            s: Box::new(rewrite(*s, f)),
            start: Box::new(rewrite(*start, f)),
            end: Box::new(rewrite(*end, f)),
        },
        HExpr::Split { s, sep } => HExpr::Split {
            s: Box::new(rewrite(*s, f)),
            sep: Box::new(rewrite(*sep, f)),
        },
        HExpr::Join { sep, xs } => HExpr::Join {
            sep: Box::new(rewrite(*sep, f)),
            xs: Box::new(rewrite(*xs, f)),
        },
        HExpr::Conj { v, x, elem } => HExpr::Conj {
            v: Box::new(rewrite(*v, f)),
            x: Box::new(rewrite(*x, f)),
            elem,
        },
        HExpr::Subvec {
            v,
            start,
            end,
            elem,
        } => HExpr::Subvec {
            v: Box::new(rewrite(*v, f)),
            start: Box::new(rewrite(*start, f)),
            end: Box::new(rewrite(*end, f)),
            elem,
        },
        HExpr::For {
            el,
            src,
            elem,
            body,
            res_elem,
        } => HExpr::For {
            el,
            src: Box::new(rewrite(*src, f)),
            elem,
            body: Box::new(rewrite(*body, f)),
            res_elem,
        },
        HExpr::Get {
            src,
            idx,
            default,
            ty,
        } => HExpr::Get {
            src: Box::new(rewrite(*src, f)),
            idx: Box::new(rewrite(*idx, f)),
            default: default.map(|d| Box::new(rewrite(*d, f))),
            ty,
        },
        HExpr::Clone(a) => HExpr::Clone(Box::new(rewrite(*a, f))),
        HExpr::DropStmt(a) => HExpr::DropStmt(Box::new(rewrite(*a, f))),
        HExpr::Ctor { adt, variant, args } => HExpr::Ctor {
            adt,
            variant,
            args: args.into_iter().map(|a| rewrite(a, f)).collect(),
        },
        HExpr::Match { scrut, arms, ty } => HExpr::Match {
            scrut: Box::new(rewrite(*scrut, f)),
            arms: arms
                .into_iter()
                .map(|a| HArm {
                    variant: a.variant,
                    binders: a.binders,
                    body: rewrite(a.body, f),
                })
                .collect(),
            ty,
        },
        HExpr::Closure {
            sig,
            mangled,
            params,
            captures,
            body,
        } => HExpr::Closure {
            sig,
            mangled,
            params,
            captures,
            body: Box::new(rewrite(*body, f)),
        },
        HExpr::ClosureCall { f: fv, args, sig } => HExpr::ClosureCall {
            f: Box::new(rewrite(*fv, f)),
            args: args.into_iter().map(|a| rewrite(a, f)).collect(),
            sig,
        },
        HExpr::Reduce(r) => HExpr::Reduce(Box::new(rewrite_reduce(*r, f))),
    };
    f(e)
}

fn rewrite_source(s: HSource, f: &mut dyn FnMut(HExpr) -> HExpr) -> HSource {
    match s {
        HSource::Var(v) => HSource::Var(v),
        HSource::Lit(vl) => HSource::Lit(vl),
        HSource::Range(a, b) => {
            HSource::Range(Box::new(rewrite(*a, f)), Box::new(rewrite(*b, f)))
        }
        HSource::Zip(a, b) => HSource::Zip(
            Box::new(rewrite_source(*a, f)),
            Box::new(rewrite_source(*b, f)),
        ),
        HSource::Take(n, inner) => {
            HSource::Take(Box::new(rewrite(*n, f)), Box::new(rewrite_source(*inner, f)))
        }
        HSource::Drop(n, inner) => {
            HSource::Drop(Box::new(rewrite(*n, f)), Box::new(rewrite_source(*inner, f)))
        }
    }
}

fn rewrite_reduce(r: HReduce, f: &mut dyn FnMut(HExpr) -> HExpr) -> HReduce {
    match r {
        HReduce::Pipeline {
            combine,
            init,
            maps,
            filters,
            src,
            parallel,
        } => HReduce::Pipeline {
            combine,
            init: rewrite(init, f),
            maps: maps
                .into_iter()
                .map(|m| MapSpec {
                    el: m.el,
                    el2: m.el2,
                    el2_ty: m.el2_ty,
                    body: rewrite(m.body, f),
                    el_ty: m.el_ty,
                    res_ty: m.res_ty,
                })
                .collect(),
            filters: filters
                .into_iter()
                .map(|fl| FilterSpec {
                    el: fl.el,
                    el2: fl.el2,
                    el2_ty: fl.el2_ty,
                    body: rewrite(fl.body, f),
                    el_ty: fl.el_ty,
                })
                .collect(),
            src: rewrite_source(src, f),
            parallel,
        },
        HReduce::Closure {
            init,
            src,
            acc,
            el,
            el2,
            body,
            maps,
            filters,
        } => HReduce::Closure {
            init: rewrite(init, f),
            src: rewrite_source(src, f),
            acc,
            el,
            el2,
            body: rewrite(body, f),
            maps: maps
                .into_iter()
                .map(|m| MapSpec {
                    el: m.el,
                    el2: m.el2,
                    el2_ty: m.el2_ty,
                    body: rewrite(m.body, f),
                    el_ty: m.el_ty,
                    res_ty: m.res_ty,
                })
                .collect(),
            filters: filters
                .into_iter()
                .map(|fl| FilterSpec {
                    el: fl.el,
                    el2: fl.el2,
                    el2_ty: fl.el2_ty,
                    body: rewrite(fl.body, f),
                    el_ty: fl.el_ty,
                })
                .collect(),
        },
    }
}

// ---------------------------------------------------------------------------
// Use counting with shadowing
// ---------------------------------------------------------------------------

/// Count references to each of `targets` inside `exprs`, resolving names
/// through the binder scopes introduced *within* those expressions: a use
/// under a re-binding of the same name counts for the inner binder, not the
/// target. Binders outside `exprs` (params, captures, loop element names)
/// are opaque boundaries.
pub fn count_uses(targets: &[String], exprs: &[&HExpr]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = targets.iter().map(|t| (t.clone(), 0)).collect();
    let mut shadowed: Vec<String> = Vec::new();
    for e in exprs {
        count_walk(e, targets, &mut shadowed, &mut counts);
    }
    counts
}

fn is_target(name: &str, targets: &[String]) -> bool {
    targets.iter().any(|t| t == name)
}

fn count_walk(
    e: &HExpr,
    targets: &[String],
    shadowed: &mut Vec<String>,
    counts: &mut HashMap<String, usize>,
) {
    // Push `name` on the shadow stack when it is one of the targets; the
    // returned bool says whether to pop.
    let enter = |name: &str, shadowed: &mut Vec<String>| -> bool {
        if is_target(name, targets) {
            shadowed.push(name.to_string());
            true
        } else {
            false
        }
    };
    match e {
        HExpr::Var(v) => {
            if is_target(v, targets) && !shadowed.iter().any(|s| s == v) {
                *counts.entry(v.clone()).or_insert(0) += 1;
            }
        }
        HExpr::ConstI64(_) | HExpr::ConstB(_) | HExpr::ConstF64(_) | HExpr::StrLit(_)
        | HExpr::VecLit(_) => {}
        HExpr::Bin(_, a, b) | HExpr::BinW(_, a, b, _) | HExpr::BinF(_, a, b) => {
            count_walk(a, targets, shadowed, counts);
            count_walk(b, targets, shadowed, counts);
        }
        HExpr::Cmp(_, a, b, _)
        | HExpr::CmpW(_, a, b, _)
        | HExpr::Min(a, b, _)
        | HExpr::Max(a, b, _)
        | HExpr::And(a, b)
        | HExpr::Or(a, b) => {
            count_walk(a, targets, shadowed, counts);
            count_walk(b, targets, shadowed, counts);
        }
        HExpr::ConvF64(a)
        | HExpr::Count(a)
        | HExpr::Not(a)
        | HExpr::ICast(a, _)
        | HExpr::Abs(a, _)
        | HExpr::Clone(a)
        | HExpr::DropStmt(a) => count_walk(a, targets, shadowed, counts),
        HExpr::StrCmp(_, a, b) => {
            count_walk(a, targets, shadowed, counts);
            count_walk(b, targets, shadowed, counts);
        }
        HExpr::StrOrd(_, a, b) => {
            count_walk(a, targets, shadowed, counts);
            count_walk(b, targets, shadowed, counts);
        }
        HExpr::StrNth { s, idx } => {
            count_walk(s, targets, shadowed, counts);
            count_walk(idx, targets, shadowed, counts);
        }
        HExpr::CharStr(c) => count_walk(c, targets, shadowed, counts),
        HExpr::Args => {}
        HExpr::Loop { binds, body, .. } => {
            for (_, _, e) in binds {
                count_walk(e, targets, shadowed, counts);
            }
            count_walk(body, targets, shadowed, counts);
        }
        HExpr::Recur(args) => {
            for a in args {
                count_walk(a, targets, shadowed, counts);
            }
        }
        HExpr::ToStr(e, _) => count_walk(e, targets, shadowed, counts),
        HExpr::ParseNum { s, .. } => count_walk(s, targets, shadowed, counts),
        HExpr::Slurp(p) => count_walk(p, targets, shadowed, counts),
        HExpr::Spit { path, data } => {
            count_walk(path, targets, shadowed, counts);
            count_walk(data, targets, shadowed, counts);
        }
        HExpr::Exit(c) => count_walk(c, targets, shadowed, counts),
        HExpr::If(c, t, f, _) => {
            count_walk(c, targets, shadowed, counts);
            count_walk(t, targets, shadowed, counts);
            count_walk(f, targets, shadowed, counts);
        }
        HExpr::Call(_, args) | HExpr::StrCat(args) | HExpr::SetLit(args) => {
            for a in args {
                count_walk(a, targets, shadowed, counts);
            }
        }
        HExpr::MapLit(pairs) => {
            for (k, v) in pairs {
                count_walk(k, targets, shadowed, counts);
                count_walk(v, targets, shadowed, counts);
            }
        }
        HExpr::MapAssoc { m, k, v } => {
            count_walk(m, targets, shadowed, counts);
            count_walk(k, targets, shadowed, counts);
            count_walk(v, targets, shadowed, counts);
        }
        HExpr::MapGet { src, key, default } => {
            count_walk(src, targets, shadowed, counts);
            count_walk(key, targets, shadowed, counts);
            if let Some(d) = default {
                count_walk(d, targets, shadowed, counts);
            }
        }
        HExpr::SetConj { s, x } | HExpr::SetDisj { s, x } | HExpr::SetContains { s, x } => {
            count_walk(s, targets, shadowed, counts);
            count_walk(x, targets, shadowed, counts);
        }
        HExpr::Subs { s, start, end } => {
            count_walk(s, targets, shadowed, counts);
            count_walk(start, targets, shadowed, counts);
            count_walk(end, targets, shadowed, counts);
        }
        HExpr::Split { s, sep } => {
            count_walk(s, targets, shadowed, counts);
            count_walk(sep, targets, shadowed, counts);
        }
        HExpr::Join { sep, xs } => {
            count_walk(sep, targets, shadowed, counts);
            count_walk(xs, targets, shadowed, counts);
        }
        HExpr::Conj { v, x, .. } => {
            count_walk(v, targets, shadowed, counts);
            count_walk(x, targets, shadowed, counts);
        }
        HExpr::Subvec { v, start, end, .. } => {
            count_walk(v, targets, shadowed, counts);
            count_walk(start, targets, shadowed, counts);
            count_walk(end, targets, shadowed, counts);
        }
        HExpr::For { el, src, body, .. } => {
            count_walk(src, targets, shadowed, counts);
            let pushed = enter(el, shadowed);
            count_walk(body, targets, shadowed, counts);
            if pushed {
                shadowed.pop();
            }
        }
        HExpr::Get {
            src,
            idx,
            default,
            ..
        } => {
            count_walk(src, targets, shadowed, counts);
            count_walk(idx, targets, shadowed, counts);
            if let Some(d) = default {
                count_walk(d, targets, shadowed, counts);
            }
        }
        HExpr::Ctor { args, .. } => {
            for a in args {
                count_walk(a, targets, shadowed, counts);
            }
        }
        HExpr::Match { scrut, arms, .. } => {
            count_walk(scrut, targets, shadowed, counts);
            for arm in arms {
                let pushed: usize = arm
                    .binders
                    .iter()
                    .map(|b| enter(b, shadowed) as usize)
                    .sum();
                count_walk(&arm.body, targets, shadowed, counts);
                for _ in 0..pushed {
                    shadowed.pop();
                }
            }
        }
        HExpr::Closure { params, captures, body, .. } => {
            // Captures are references to the enclosing scope.
            for (n, _) in captures {
                count_walk(&HExpr::Var(n.clone()), targets, shadowed, counts);
            }
            let pushed: usize = params
                .iter()
                .map(|(n, _)| enter(n, shadowed) as usize)
                .sum();
            count_walk(body, targets, shadowed, counts);
            for _ in 0..pushed {
                shadowed.pop();
            }
        }
        HExpr::ClosureCall { f: fv, args, .. } => {
            count_walk(fv, targets, shadowed, counts);
            for a in args {
                count_walk(a, targets, shadowed, counts);
            }
        }
        HExpr::Let(binds, body, _) => {
            let mut pushed = 0;
            for (n, init, _) in binds {
                count_walk(init, targets, shadowed, counts);
                if enter(n, shadowed) {
                    pushed += 1;
                }
            }
            count_walk(body, targets, shadowed, counts);
            for _ in 0..pushed {
                shadowed.pop();
            }
        }
        HExpr::Seq(vs) => {
            for v in vs {
                count_walk(v, targets, shadowed, counts);
            }
        }
        HExpr::Reduce(r) => count_reduce(r, targets, shadowed, counts),
    }
}

fn count_reduce(
    r: &HReduce,
    targets: &[String],
    shadowed: &mut Vec<String>,
    counts: &mut HashMap<String, usize>,
) {
    // Loop element names (`el`, `acc`) are implicit binders; uses inside
    // stage bodies that re-bind a target name must not count for it, so
    // push them on the shadow stack conservatively.
    match r {
        HReduce::Pipeline { init, maps, filters, src, .. } => {
            count_walk(init, targets, shadowed, counts);
            count_source(src, targets, shadowed, counts);
            // Stage element names bind for their own body and every later
            // stage: push each binder, walk its body, pop everything after.
            let mut pushed = 0;
            for m in maps {
                pushed += enter_el(&m.el, targets, shadowed);
                count_walk(&m.body, targets, shadowed, counts);
            }
            for fl in filters {
                pushed += enter_el(&fl.el, targets, shadowed);
                count_walk(&fl.body, targets, shadowed, counts);
            }
            for _ in 0..pushed {
                shadowed.pop();
            }
        }
        HReduce::Closure { init, src, body, maps, filters, el, .. } => {
            count_walk(init, targets, shadowed, counts);
            count_source(src, targets, shadowed, counts);
            let mut pushed = enter_el(el, targets, shadowed);
            for m in maps {
                pushed += enter_el(&m.el, targets, shadowed);
                count_walk(&m.body, targets, shadowed, counts);
            }
            for fl in filters {
                pushed += enter_el(&fl.el, targets, shadowed);
                count_walk(&fl.body, targets, shadowed, counts);
            }
            count_walk(body, targets, shadowed, counts);
            for _ in 0..pushed {
                shadowed.pop();
            }
        }
    }
}

fn enter_el(el: &str, targets: &[String], shadowed: &mut Vec<String>) -> usize {
    if is_target(el, targets) {
        shadowed.push(el.to_string());
        1
    } else {
        0
    }
}

fn count_source(
    s: &HSource,
    targets: &[String],
    shadowed: &mut Vec<String>,
    counts: &mut HashMap<String, usize>,
) {
    match s {
        HSource::Var(v) => count_walk(&HExpr::Var(v.clone()), targets, shadowed, counts),
        HSource::Lit(_) => {}
        HSource::Range(a, b) => {
            count_walk(a, targets, shadowed, counts);
            count_walk(b, targets, shadowed, counts);
        }
        HSource::Zip(a, b) => {
            count_source(a, targets, shadowed, counts);
            count_source(b, targets, shadowed, counts);
        }
        HSource::Take(n, inner) | HSource::Drop(n, inner) => {
            count_walk(n, targets, shadowed, counts);
            count_source(inner, targets, shadowed, counts);
        }
    }
}

// ---------------------------------------------------------------------------
// Scoped substitution
// ---------------------------------------------------------------------------

/// Replace every unshadowed `Var(name)` in `e` with a clone of `replacement`.
/// Conservative on `Reduce` nodes (implicit loop-element binders make the
/// scoping of stage bodies pass-local): substitutions there are skipped.
pub fn substitute(e: HExpr, name: &str, replacement: &HExpr) -> HExpr {
    match e {
        HExpr::Var(v) if v == name => replacement.clone(),
        HExpr::Let(binds, body, frees) => {
            let mut binds = binds;
            let mut shadowed_at = None;
            for (i, (n, init, _)) in binds.iter_mut().enumerate() {
                if n == name {
                    shadowed_at = Some(i);
                    break;
                }
                let taken = std::mem::replace(init, HExpr::ConstI64(0));
                *init = substitute(taken, name, replacement);
            }
            if shadowed_at.is_some() {
                // Shadowing binder found: inits from there on and the body
                // are out of scope for `name`.
                HExpr::Let(binds, body, frees)
            } else {
                HExpr::Let(binds, Box::new(substitute(*body, name, replacement)), frees)
            }
        }
        HExpr::Match { scrut, arms, ty } => HExpr::Match {
            scrut: Box::new(substitute(*scrut, name, replacement)),
            arms: arms
                .into_iter()
                .map(|a| {
                    if a.binders.iter().any(|b| b == name) {
                        a
                    } else {
                        HArm {
                            variant: a.variant,
                            binders: a.binders,
                            body: substitute(a.body, name, replacement),
                        }
                    }
                })
                .collect(),
            ty,
        },
        HExpr::Closure {
            sig,
            mangled,
            params,
            captures,
            body,
        } => {
            let shadows = params.iter().any(|(p, _)| p == name);
            HExpr::Closure {
                sig,
                mangled,
                captures,
                params,
                body: if shadows {
                    body
                } else {
                    Box::new(substitute(*body, name, replacement))
                },
            }
        },
        HExpr::For {
            el,
            src,
            elem,
            body,
            res_elem,
        } => {
            let shadows = el == name;
            HExpr::For {
                el,
                src: Box::new(substitute(*src, name, replacement)),
                elem,
                res_elem,
                body: if shadows {
                    body
                } else {
                    Box::new(substitute(*body, name, replacement))
                },
            }
        },
        HExpr::Reduce(_) => e,
        // Every remaining kind introduces no binders: substitute plainly.
        other => substitute_simple(other, name, replacement),
    }
}

/// Substitute through the remaining node kinds, which introduce no binders.
fn substitute_simple(e: HExpr, name: &str, replacement: &HExpr) -> HExpr {
    rewrite(e, &mut |node| match node {
        HExpr::Var(v) if v == name => replacement.clone(),
        other => other,
    })
}

// ---------------------------------------------------------------------------
// Purity
// ---------------------------------------------------------------------------

/// Can this expression be deleted without changing observable behavior?
/// No printing, no freeing, no observable mutation. Allocations are fine
/// (dropping one only avoids work). Conservative on everything that calls.
pub fn is_drop_safe(e: &HExpr) -> bool {
    match e {
        HExpr::ConstI64(_)
        | HExpr::ConstF64(_)
        | HExpr::Var(_)
        | HExpr::StrLit(_)
        | HExpr::VecLit(_)
        | HExpr::Bin(..)
        | HExpr::BinW(..)
        | HExpr::BinF(..)
        | HExpr::Cmp(..)
        | HExpr::CmpW(..)
        | HExpr::ConvF64(_)
        | HExpr::ICast(..)
        | HExpr::Not(_)
        | HExpr::Abs(..)
        | HExpr::Min(..)
        | HExpr::Max(..)
        | HExpr::Count(_)
        | HExpr::StrCmp(..)
        | HExpr::StrOrd(..)
        | HExpr::Clone(_)
        | HExpr::Get { .. }
        | HExpr::MapLit(_)
        | HExpr::MapAssoc { .. }
        | HExpr::MapGet { .. }
        | HExpr::SetLit(_)
        | HExpr::SetConj { .. }
        | HExpr::SetDisj { .. }
        | HExpr::SetContains { .. }
        | HExpr::Subs { .. }
        | HExpr::Split { .. }
        | HExpr::Join { .. }
        | HExpr::Subvec { .. }
        | HExpr::Ctor { .. } => true,
        HExpr::And(a, b) | HExpr::Or(a, b) => is_drop_safe(a) && is_drop_safe(b),
        HExpr::StrCat(args) => args.iter().all(is_drop_safe),
        HExpr::Seq(vs) => vs.iter().all(is_drop_safe),
        HExpr::Let(binds, body, _) => {
            binds.iter().all(|(_, e, _)| is_drop_safe(e)) && is_drop_safe(body)
        }
        // Everything else is either effectful (Call, Conj, DropStmt, For),
        // control flow (If, Match), or a loop whose shape codegen depends
        // on (Reduce).
        _ => false,
    }
}

/// Can this expression be *duplicated* without changing observable behavior
/// or leaking? Strictly weaker set than `is_drop_safe`: no allocations.
pub fn is_dup_safe(e: &HExpr) -> bool {
    match e {
        HExpr::ConstI64(_)
        | HExpr::ConstF64(_)
        | HExpr::Var(_)
        | HExpr::StrLit(_)
        | HExpr::Bin(..)
        | HExpr::BinW(..)
        | HExpr::BinF(..)
        | HExpr::Cmp(..)
        | HExpr::CmpW(..)
        | HExpr::ConvF64(_)
        | HExpr::ICast(..)
        | HExpr::Not(_)
        | HExpr::Abs(..)
        | HExpr::Min(..)
        | HExpr::Max(..)
        | HExpr::Count(_)
        | HExpr::StrCmp(..)
        | HExpr::StrOrd(..)
        | HExpr::Get { .. } => true,
        HExpr::And(a, b) | HExpr::Or(a, b) => is_dup_safe(a) && is_dup_safe(b),
        HExpr::StrCat(args) => args.iter().all(is_dup_safe),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

/// Number of nodes in the expression tree.
pub fn node_count(e: &HExpr) -> usize {
    let mut n = 0usize;
    visit_nodes(e, &mut |_| n += 1);
    n
}

/// Does the tree contain a `Reduce` anywhere?
pub fn contains_reduce(e: &HExpr) -> bool {
    let mut found = false;
    visit_nodes(e, &mut |n| {
        if matches!(n, HExpr::Reduce(_)) {
            found = true;
        }
    });
    found
}

/// Every name that appears in any `Let` free list of the tree.
pub fn all_frees_names(e: &HExpr, out: &mut HashSet<String>) {
    visit_nodes(e, &mut |n| {
        if let HExpr::Let(_, _, frees) = n {
            for name in frees {
                out.insert(name.clone());
            }
        }
    });
}

/// Every name referenced by a `Var` anywhere in the tree (an over-
/// approximation of free variables; shadowed references included).
pub fn all_var_names(e: &HExpr, out: &mut HashSet<String>) {
    visit_nodes(e, &mut |n| {
        if let HExpr::Var(v) = n {
            out.insert(v.clone());
        }
    });
}

/// Does the tree contain any `Var` reference at all?
pub fn has_var_refs(e: &HExpr) -> bool {
    let mut found = false;
    visit_nodes(e, &mut |n| {
        if matches!(n, HExpr::Var(_)) {
            found = true;
        }
    });
    found
}

/// Does the tree contain a direct `Call` to `name`?
pub fn calls_name(e: &HExpr, name: &str) -> bool {
    let mut found = false;
    visit_nodes(e, &mut |n| {
        if matches!(n, HExpr::Call(c, _) if c == name) {
            found = true;
        }
    });
    found
}

/// Read-only walk over every node (including inside `Reduce` specs and
/// `Match` arms); used by analyses that need the whole tree.
pub fn visit_nodes(e: &HExpr, f: &mut dyn FnMut(&HExpr)) {
    f(e);
    match e {
        HExpr::ConstI64(_)
        | HExpr::ConstB(_)
        | HExpr::ConstF64(_)
        | HExpr::Var(_)
        | HExpr::StrLit(_)
        | HExpr::VecLit(_) => {}
        HExpr::Bin(_, a, b) | HExpr::BinW(_, a, b, _) | HExpr::BinF(_, a, b) => {
            visit_nodes(a, f);
            visit_nodes(b, f);
        }
        HExpr::Cmp(_, a, b, _)
        | HExpr::CmpW(_, a, b, _)
        | HExpr::Min(a, b, _)
        | HExpr::Max(a, b, _)
        | HExpr::And(a, b)
        | HExpr::Or(a, b) => {
            visit_nodes(a, f);
            visit_nodes(b, f);
        }
        HExpr::ConvF64(a)
        | HExpr::Count(a)
        | HExpr::Not(a)
        | HExpr::ICast(a, _)
        | HExpr::Abs(a, _)
        | HExpr::Clone(a)
        | HExpr::DropStmt(a) => visit_nodes(a, f),
        HExpr::StrCmp(_, a, b) => {
            visit_nodes(a, f);
            visit_nodes(b, f);
        }
        HExpr::StrOrd(_, a, b) => {
            visit_nodes(a, f);
            visit_nodes(b, f);
        }
        HExpr::StrNth { s, idx } => {
            visit_nodes(s, f);
            visit_nodes(idx, f);
        }
        HExpr::CharStr(c) => visit_nodes(c, f),
        HExpr::Args => {}
        HExpr::Loop { binds, body, .. } => {
            for (_, _, e) in binds {
                visit_nodes(e, f);
            }
            visit_nodes(body, f);
        }
        HExpr::Recur(args) => {
            for a in args {
                visit_nodes(a, f);
            }
        }
        HExpr::ToStr(e, _) => visit_nodes(e, f),
        HExpr::ParseNum { s, .. } => visit_nodes(s, f),
        HExpr::Slurp(p) => visit_nodes(p, f),
        HExpr::Spit { path, data } => {
            visit_nodes(path, f);
            visit_nodes(data, f);
        }
        HExpr::Exit(c) => visit_nodes(c, f),
        HExpr::If(c, t, f2, _) => {
            visit_nodes(c, f);
            visit_nodes(t, f);
            visit_nodes(f2, f);
        }
        HExpr::Call(_, args) | HExpr::StrCat(args) | HExpr::SetLit(args) => {
            for a in args {
                visit_nodes(a, f);
            }
        }
        HExpr::MapLit(pairs) => {
            for (k, v) in pairs {
                visit_nodes(k, f);
                visit_nodes(v, f);
            }
        }
        HExpr::MapAssoc { m, k, v } => {
            visit_nodes(m, f);
            visit_nodes(k, f);
            visit_nodes(v, f);
        }
        HExpr::MapGet { src, key, default } => {
            visit_nodes(src, f);
            visit_nodes(key, f);
            if let Some(d) = default {
                visit_nodes(d, f);
            }
        }
        HExpr::SetConj { s, x } | HExpr::SetDisj { s, x } | HExpr::SetContains { s, x } => {
            visit_nodes(s, f);
            visit_nodes(x, f);
        }
        HExpr::Subs { s, start, end } => {
            visit_nodes(s, f);
            visit_nodes(start, f);
            visit_nodes(end, f);
        }
        HExpr::Split { s, sep } => {
            visit_nodes(s, f);
            visit_nodes(sep, f);
        }
        HExpr::Join { sep, xs } => {
            visit_nodes(sep, f);
            visit_nodes(xs, f);
        }
        HExpr::Conj { v, x, .. } => {
            visit_nodes(v, f);
            visit_nodes(x, f);
        }
        HExpr::Subvec { v, start, end, .. } => {
            visit_nodes(v, f);
            visit_nodes(start, f);
            visit_nodes(end, f);
        }
        HExpr::For { src, body, .. } => {
            visit_nodes(src, f);
            visit_nodes(body, f);
        }
        HExpr::Get {
            src,
            idx,
            default,
            ..
        } => {
            visit_nodes(src, f);
            visit_nodes(idx, f);
            if let Some(d) = default {
                visit_nodes(d, f);
            }
        }
        HExpr::Ctor { args, .. } => {
            for a in args {
                visit_nodes(a, f);
            }
        }
        HExpr::Match { scrut, arms, .. } => {
            visit_nodes(scrut, f);
            for arm in arms {
                visit_nodes(&arm.body, f);
            }
        }
        HExpr::Closure { body, .. } => visit_nodes(body, f),
        HExpr::ClosureCall { f: fv, args, .. } => {
            visit_nodes(fv, f);
            for a in args {
                visit_nodes(a, f);
            }
        }
        HExpr::Let(binds, body, _) => {
            for (_, init, _) in binds {
                visit_nodes(init, f);
            }
            visit_nodes(body, f);
        }
        HExpr::Seq(vs) => {
            for v in vs {
                visit_nodes(v, f);
            }
        }
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline {
                init,
                maps,
                filters,
                src,
                ..
            } => {
                visit_nodes(init, f);
                visit_source(src, f);
                for m in maps {
                    visit_nodes(&m.body, f);
                }
                for fl in filters {
                    visit_nodes(&fl.body, f);
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
                visit_nodes(init, f);
                visit_source(src, f);
                visit_nodes(body, f);
                for m in maps {
                    visit_nodes(&m.body, f);
                }
                for fl in filters {
                    visit_nodes(&fl.body, f);
                }
            }
        },
    }
}

fn visit_source(s: &HSource, f: &mut dyn FnMut(&HExpr)) {
    match s {
        HSource::Var(_) | HSource::Lit(_) => {}
        HSource::Range(a, b) => {
            visit_nodes(a, f);
            visit_nodes(b, f);
        }
        HSource::Zip(a, b) => {
            visit_source(a, f);
            visit_source(b, f);
        }
        HSource::Take(n, inner) | HSource::Drop(n, inner) => {
            visit_nodes(n, f);
            visit_source(inner, f);
        }
    }
}

/// Every binder name introduced anywhere in `e` (lets, match arms, closure
/// params). Used to mint collision-free fresh names.
pub fn all_binders(e: &HExpr, out: &mut HashSet<String>) {
    visit_nodes(e, &mut |n| match n {
        HExpr::Let(binds, _, _) => {
            for (name, _, _) in binds {
                out.insert(name.clone());
            }
        }
        HExpr::Match { arms, .. } => {
            for arm in arms {
                for b in &arm.binders {
                    out.insert(b.clone());
                }
            }
        }
        HExpr::Closure { params, .. } => {
            for (p, _) in params {
                out.insert(p.clone());
            }
        }
        _ => {}
    });
}
