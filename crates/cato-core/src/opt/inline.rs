//! Function inlining at call sites.
//!
//! Candidates are small, non-recursive, non-extern defns whose params are
//! all substitution-safe (numeric/bool/str — never heap-owning or ADT, so
//! substitution cannot alias a freed buffer or double-move). Pure args are
//! substituted directly; impure args are bound with a fresh `let` so their
//! eager left-to-right evaluation at the call site is preserved.
//!
//! Templates are closed at the defn level (no free variables), so the only
//! capture hazard is a template binder shadowing a caller variable that an
//! arg references — such call sites are skipped rather than renamed.

use super::walk::{
    all_binders, all_var_names, count_uses, is_drop_safe, is_dup_safe, node_count, rewrite,
};
use crate::hir::*;
use crate::Program;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};

const INLINE_BUDGET: usize = 48;

struct Candidate {
    params: Vec<(String, Ty)>,
    body: HExpr,
    /// Uses of each param in the body.
    uses: HashMap<String, usize>,
    binder_names: HashSet<String>,
}

pub fn run(program: &mut Program) -> bool {
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    for d in program.defns.iter() {
        if d.is_extern
            || d.name == "main"
            || !substitution_safe_params(&d.params)
            || !substitution_safe_ret(d.ret)
            || node_count(&d.body) > INLINE_BUDGET
            || super::walk::contains_reduce(&d.body)
            || calls_directly(&d.body, &d.name)
            || captures_param(&d.body, &d.params)
        {
            continue;
        }
        let param_names: Vec<String> = d.params.iter().map(|(n, _)| n.clone()).collect();
        let uses = count_uses(&param_names, &[&d.body]);
        let mut binder_names = HashSet::new();
        all_binders(&d.body, &mut binder_names);
        for (n, _) in &d.params {
            binder_names.insert(n.clone());
        }
        candidates.insert(
            d.name.clone(),
            Candidate {
                params: d.params.clone(),
                body: d.body.clone(),
                uses,
                binder_names,
            },
        );
    }
    if candidates.is_empty() {
        return false;
    }

    let mut binder_universe: HashSet<String> = HashSet::new();
    for d in &program.defns {
        all_binders(&d.body, &mut binder_universe);
    }
    for lam in &program.lambdas {
        all_binders(&lam.body, &mut binder_universe);
    }

    let changed = Cell::new(false);
    let counter = Cell::new(0usize);

    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        let cur = d.name.clone();
        let taken = std::mem::replace(&mut d.body, HExpr::ConstI64(0));
        d.body = rewrite(taken, &mut |e| {
            expand_site(e, &cur, &candidates, &binder_universe, &counter, &changed)
        });
    }
    for lam in program.lambdas.iter_mut() {
        let taken = std::mem::replace(&mut lam.body, HExpr::ConstI64(0));
        lam.body = rewrite(taken, &mut |e| {
            expand_site(e, "", &candidates, &binder_universe, &counter, &changed)
        });
    }
    changed.get()
}

fn substitution_safe_params(params: &[(String, Ty)]) -> bool {
    params.iter().all(|(_, t)| {
        t.is_numeric() || matches!(t, Ty::Bool | Ty::Str)
    })
}

/// The substituted body value takes the call site's uses across destruction
/// points computed for the call result, so a heap-owning return must not be
/// inlined either (its buffer could be freed while a re-emitted use of the
/// original binding still reads it).
fn substitution_safe_ret(ret: Ty) -> bool {
    ret.is_copy_ty()
}

fn calls_directly(e: &HExpr, name: &str) -> bool {
    super::walk::calls_name(e, name)
}

/// Does any closure in the body capture one of these params? Capture slots
/// hold names, not expressions, so substitution cannot rewrite them —
/// such defns are not inlinable.
fn captures_param(e: &HExpr, params: &[(String, Ty)]) -> bool {
    let names: Vec<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
    let mut found = false;
    super::walk::visit_nodes(e, &mut |n| {
        if let HExpr::Closure { captures, .. } = n {
            for (c, _) in captures {
                if names.contains(&c.as_str()) {
                    found = true;
                }
            }
        }
    });
    found
}

fn expand_site(
    e: HExpr,
    cur: &str,
    candidates: &HashMap<String, Candidate>,
    binder_universe: &HashSet<String>,
    counter: &Cell<usize>,
    changed: &Cell<bool>,
) -> HExpr {
    let HExpr::Call(name, args) = e else {
        return e;
    };
    if name == cur {
        return HExpr::Call(name, args);
    }
    let Some(c) = candidates.get(&name) else {
        return HExpr::Call(name, args);
    };
    if args.len() != c.params.len() {
        return HExpr::Call(name, args);
    }

    // Capture guard: any caller variable referenced by an arg must not share
    // a name with a template binder.
    let mut arg_vars: HashSet<String> = HashSet::new();
    for a in &args {
        all_var_names(a, &mut arg_vars);
    }
    if arg_vars.iter().any(|v| c.binder_names.contains(v)) {
        return HExpr::Call(name, args);
    }

    // Per-param feasibility.
    for (i, (pname, _)) in c.params.iter().enumerate() {
        let n_uses = c.uses.get(pname).copied().unwrap_or(0);
        let arg = &args[i];
        match n_uses {
            0 => {
                if !is_drop_safe(arg) {
                    return HExpr::Call(name, args);
                }
            }
            1 => {}
            _ => {
                if !is_dup_safe(arg) {
                    return HExpr::Call(name, args);
                }
            }
        }
    }

    // Build the inlined body: bind impure args eagerly (in call order), then
    // substitute everything into the template.
    let mut template = c.body.clone();
    let mut impure_binds: Vec<(String, HExpr, Ty)> = Vec::new();
    for (i, (pname, pty)) in c.params.iter().enumerate() {
        let n_uses = c.uses.get(pname).copied().unwrap_or(0);
        if n_uses == 0 {
            continue;
        }
        let arg = args[i].clone();
        let replacement = if is_drop_safe(&arg) {
            arg
        } else {
            let fresh = mint_fresh(pname, binder_universe, counter);
            impure_binds.push((fresh.clone(), arg, *pty));
            HExpr::Var(fresh)
        };
        let taken = std::mem::replace(&mut template, HExpr::ConstI64(0));
        template = super::walk::substitute(taken, pname, &replacement);
    }

    changed.set(true);
    if impure_binds.is_empty() {
        template
    } else {
        HExpr::Let(impure_binds, Box::new(template), Vec::new())
    }
}

fn mint_fresh(base: &str, universe: &HashSet<String>, counter: &Cell<usize>) -> String {
    loop {
        counter.set(counter.get() + 1);
        let candidate = format!("__inl{}_{}", counter.get(), base);
        if !universe.contains(&candidate) {
            return candidate;
        }
    }
}
