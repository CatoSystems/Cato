//! Clone elision: `(clone x)` where `x` is never used again becomes `x`,
//! and the source binding's scope-exit free entry (if any) is removed —
//! ownership over the buffer transfers to the clone's destination.
//!
//! In an immutable-value language the only observable difference between a
//! value and its clone is buffer identity, and buffer identity only matters
//! through the scope-exit free lists: removing the source's entry keeps
//! exactly one free per surviving buffer (a removed entry can only turn a
//! would-be free into a leak, never a double free).
//!
//! Eligibility uses plain occurrence counting, sound because candidates
//! are names bound exactly once in the body (no shadow resolution needed).
//! Runs after the final free-list recomputation; nothing re-annotates
//! afterwards.

use super::walk::{rewrite, visit_nodes};
use crate::hir::*;
use crate::Program;
use std::collections::{HashMap, HashSet};

pub fn run(program: &mut Program) -> bool {
    let mut changed = false;
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        changed |= elide_in_body(&mut d.body);
    }
    for lam in program.lambdas.iter_mut() {
        changed |= elide_in_body(&mut lam.body);
    }
    changed
}

fn elide_in_body(body: &mut HExpr) -> bool {
    // Clone sites by source name.
    let mut site_counts: HashMap<String, usize> = HashMap::new();
    visit_nodes(body, &mut |n| {
        if let HExpr::Clone(inner) = n {
            if let HExpr::Var(v) = &**inner {
                *site_counts.entry(v.clone()).or_insert(0) += 1;
            }
        }
    });

    // Names with an actual scope-exit free entry: only these can transfer
    // ownership. A match binder over an owned parameter, for instance, has
    // no entry of its own (its buffer is freed by the parent's drop glue) —
    // eliding its clone would share a buffer that dies with the source.
    let mut freeable: HashSet<String> = HashSet::new();
    visit_nodes(body, &mut |n| {
        if let HExpr::Let(_, _, frees, _) = n {
            for f in frees {
                freeable.insert(f.clone());
            }
        }
    });
    // Static literals bind no heap: aliasing them is always sound.
    let mut static_bound: HashSet<String> = HashSet::new();
    visit_nodes(body, &mut |n| {
        if let HExpr::Let(binds, _, _, _) = n {
            for (n2, v, _) in binds {
                if matches!(v, HExpr::VecLit(_) | HExpr::StrLit(_)) {
                    static_bound.insert(n2.clone());
                }
            }
        }
    });

    let candidates: HashSet<String> = site_counts
        .into_iter()
        .filter(|(name, sites)| {
            if *sites != 1 {
                return false;
            }
            if !freeable.contains(name) && !static_bound.contains(name) {
                return false;
            }
            // Bound exactly once (params count as zero in-body bindings) and
            // referenced exactly once: by the clone itself.
            binder_occurrences(body, name) <= 1 && var_occurrences(body, name) == 1
        })
        .map(|(name, _)| name)
        .collect();
    if candidates.is_empty() {
        return false;
    }

    let taken = std::mem::replace(body, HExpr::ConstI64(0));
    *body = rewrite(taken, &mut |e| match e {
        HExpr::Clone(inner) => match &*inner {
            HExpr::Var(v) if candidates.contains(v) => *inner,
            _ => HExpr::Clone(inner),
        },
        other => other,
    });

    // Transfer: the sources' scope-exit frees must go — the destination
    // binding (or the enclosing escape analysis) now owns the buffer.
    remove_frees_entries(body, &candidates);
    true
}

fn remove_frees_entries(e: &mut HExpr, names: &HashSet<String>) {
    let taken = std::mem::replace(e, HExpr::ConstI64(0));
    *e = rewrite(taken, &mut |node| match node {
        HExpr::Let(binds, body, frees, rebind) => HExpr::Let(
            binds,
            body,
            frees.into_iter().filter(|f| !names.contains(f)).collect(),
            // A transferred binding's shadowed-buffer frees go with its exit
            // free: the destination owns every buffer the name held.
            rebind
                .into_iter()
                .filter(|(f, _)| !names.contains(f))
                .collect(),
        ),
        other => other,
    });
}

fn binder_occurrences(e: &HExpr, name: &str) -> usize {
    let mut count = 0usize;
    visit_nodes(e, &mut |node| match node {
        HExpr::Let(binds, _, _, _) => {
            for (n, _, _) in binds {
                if n == name {
                    count += 1;
                }
            }
        }
        HExpr::Match { arms, .. } => {
            for arm in arms {
                for b in &arm.binders {
                    if b == name {
                        count += 1;
                    }
                }
            }
        }
        HExpr::Closure { params, .. } => {
            for (p, _) in params {
                if p == name {
                    count += 1;
                }
            }
        }
        HExpr::For { el, .. } => {
            if el == name {
                count += 1;
            }
        }
        HExpr::Reduce(r) => {
            let mut binders: Vec<&String> = Vec::new();
            match &**r {
                HReduce::Pipeline { maps, filters, .. } => {
                    binders.extend(maps.iter().map(|m| &m.el));
                    binders.extend(filters.iter().map(|f| &f.el));
                }
                HReduce::Closure {
                    acc,
                    el,
                    el2,
                    maps,
                    filters,
                    ..
                } => {
                    binders.push(acc);
                    binders.push(el);
                    if let Some(e2) = el2 {
                        binders.push(e2);
                    }
                    binders.extend(maps.iter().map(|m| &m.el));
                    binders.extend(filters.iter().map(|f| &f.el));
                }
            }
            for binder in binders {
                if binder == name {
                    count += 1;
                }
            }
        }
        _ => {}
    });
    count
}

fn var_occurrences(e: &HExpr, name: &str) -> usize {
    let mut count = 0usize;
    visit_nodes(e, &mut |node| match node {
        HExpr::Var(v) if v == name => count += 1,
        // Closure captures are name references (and they move non-copy
        // values), so they count as uses.
        HExpr::Closure { captures, .. } => {
            for (c, _) in captures {
                if c == name {
                    count += 1;
                }
            }
        }
        // Loop sources read their vector by name.
        HExpr::Reduce(r) => {
            let src = match &**r {
                HReduce::Pipeline { src, .. } => src,
                HReduce::Closure { src, .. } => src,
            };
            count_source_vars(src, name, &mut count);
        }
        _ => {}
    });
    count
}

fn count_source_vars(s: &HSource, name: &str, count: &mut usize) {
    match s {
        HSource::Var(v) if v == name => *count += 1,
        HSource::Var(_) | HSource::Lit(_) => {}
        HSource::Range(..) => {}
        HSource::Zip(a, b) => {
            count_source_vars(a, name, count);
            count_source_vars(b, name, count);
        }
        HSource::Take(_, inner) | HSource::Drop(_, inner) => {
            count_source_vars(inner, name, count);
        }
    }
}
