//! Parameter-mode inference: which parameters lend instead of moving.

use super::*;

/// Parameter-mode inference: *a parameter that
/// only reads its argument lends it — the caller keeps the value.* Every
/// non-Copy parameter written without `&` starts as lent; a body that keeps
/// it (`ownership::kept_params`) makes it owned, and its callers are
/// re-checked (their own parameters may now flow into an owned position).
/// Parameters only ever go lent -> owned, so the worklist terminates. Then
/// `(f (clone x))` into a lent parameter passes `x` itself.
///
/// Excluded: `main`, externs (all `&` by contract), and `&T`-returning
/// defns (their borrowed result is tied to their declared `&` params).
pub(crate) fn infer_param_modes(
    out: &mut [Defn],
    rets: &HashMap<String, Ty>,
    ret_borrowed_names: &std::collections::HashSet<String>,
    facts: &AdtFacts,
    adts: &[AdtDef],
    alias_ret: &std::collections::HashSet<String>,
) {
    use std::collections::{HashSet, VecDeque};
    let mut sigs: HashMap<String, Vec<Borrow>> = out
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    // Candidates: defn name -> parameter positions still inferred lent.
    let mut lent: HashMap<String, Vec<usize>> = HashMap::new();
    for d in out.iter() {
        if d.is_extern || d.name == "main" || d.ret_borrowed {
            continue;
        }
        let cand: Vec<usize> = d
            .params
            .iter()
            .enumerate()
            .filter(|(i, (_, t))| {
                d.borrows.get(*i).copied().unwrap_or(Borrow::Owned) == Borrow::Owned
                    && crate::hir::ownership::is_non_copy(*t)
            })
            .map(|(i, _)| i)
            .collect();
        if cand.is_empty() {
            continue;
        }
        let s = sigs.get_mut(&d.name).unwrap();
        for &i in &cand {
            s[i] = Borrow::Shared;
        }
        lent.insert(d.name.clone(), cand);
    }
    // Who calls whom: a callee's parameter turning owned re-checks callers.
    let mut callers: HashMap<String, HashSet<usize>> = HashMap::new();
    for (k, d) in out.iter().enumerate() {
        crate::opt::walk::visit_nodes(&d.body, &mut |n| {
            if let HExpr::Call(f, _) = n {
                callers.entry(f.clone()).or_default().insert(k);
            }
        });
    }
    let mut work: VecDeque<usize> = (0..out.len())
        .filter(|k| lent.contains_key(&out[*k].name))
        .collect();
    let mut queued: HashSet<usize> = work.iter().copied().collect();
    while let Some(k) = work.pop_front() {
        queued.remove(&k);
        let d = &out[k];
        let Some(cand) = lent.get(&d.name).cloned() else {
            continue;
        };
        let kept = crate::hir::ownership::kept_params(
            &d.name,
            &d.params,
            d.ret,
            d.ret_borrowed,
            &d.body,
            &sigs,
            ret_borrowed_names,
            rets,
            facts,
            adts,
            alias_ret,
        );
        let now_owned: Vec<usize> = cand.iter().copied().filter(|&i| kept[i]).collect();
        if now_owned.is_empty() {
            continue;
        }
        let name = d.name.clone();
        let s = sigs.get_mut(&name).unwrap();
        for &i in &now_owned {
            s[i] = Borrow::Owned;
        }
        lent.get_mut(&name)
            .unwrap()
            .retain(|i| !now_owned.contains(i));
        for &c in callers.get(&name).into_iter().flatten() {
            if lent.contains_key(&out[c].name) && queued.insert(c) {
                work.push_back(c);
            }
        }
    }
    for d in out.iter_mut() {
        if let Some(s) = sigs.get(&d.name) {
            d.borrows = s.clone();
        }
    }
    // A clone handed to a lent parameter is a copy nobody keeps: pass the
    // value itself (the callee only reads it during the call). Not when
    // another argument of the same call mentions it — the callee would see
    // an owned and a lent view of one object.
    for d in out.iter_mut() {
        let body = std::mem::replace(&mut d.body, HExpr::ConstI64(0));
        d.body = crate::opt::walk::rewrite(body, &mut |e| match e {
            HExpr::Call(f, mut args) => {
                if let Some(kinds) = sigs.get(&f) {
                    for i in 0..args.len() {
                        if kinds.get(i) != Some(&Borrow::Shared) {
                            continue;
                        }
                        let HExpr::Clone(inner) = &args[i] else {
                            continue;
                        };
                        let HExpr::Var(x) = &**inner else { continue };
                        let x = x.clone();
                        let elsewhere = args.iter().enumerate().any(|(j, a)| {
                            j != i && {
                                let mut m: HashMap<String, i64> = HashMap::new();
                                crate::hir::ownership::count_vars_pub(a, &mut m);
                                m.contains_key(&x)
                            }
                        });
                        if !elsewhere {
                            args[i] = HExpr::Var(x);
                        }
                    }
                }
                HExpr::Call(f, args)
            }
            other => other,
        });
    }
}
