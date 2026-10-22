//! Loop and value-flow drop planning.
//!
//! `recur` is a scope exit: everything an iteration owns is dropped at the
//! jump (after the arguments are computed), loop variables that own their
//! value on every iteration drop when replaced and on completing paths, and
//! a value stored by pointer through a merge hands over the bindings it may
//! be. Every decision is guarded by a may-alias *roots* analysis: nothing a
//! surviving value may point into is dropped. Uncertainty leaks, it never
//! frees.

use super::*;

/// One tail `recur`: the argument expressions and the bindings visible at it.
type RecurSite = (Vec<HExpr>, Vec<(String, Origin)>);

/// One open `loop` under checking. `scope` lists, innermost last, every
/// name the loop's body has bound on the path being checked — the loop
/// variables first, then `let` bindings (droppable) and match binders
/// (aliases: never droppable, but they shadow). A `recur` resolves each
/// name to its innermost entry, so a shadowed binding is never dropped
/// under a name that now means something else.
pub(super) struct LoopFrame {
    pub(super) scope: Vec<(String, bool)>,
}

/// How a name visible at a `recur` was bound (`loop_owning`'s pre-scan).
#[derive(Clone)]
pub(super) enum Origin {
    LoopVar,
    Let {
        fresh: bool,
    },
    /// A match binder; `heap`: its field is a heap object the box owns.
    Binder {
        owner: BoxOwner,
        heap: bool,
    },
}

#[derive(Clone)]
pub(super) enum BoxOwner {
    /// The matched box is this loop variable's value.
    LoopVar(String),
    /// A fresh `let` of the iteration, or a fresh temp: owned here.
    Owned,
    /// Borrowed, aliased, or from outside the loop.
    Nobody,
}

pub(super) fn resolve<'s>(scope: &'s [(String, Origin)], name: &str) -> Option<&'s Origin> {
    scope.iter().rev().find(|(n, _)| n == name).map(|(_, o)| o)
}

/// Does this `recur` argument hand the next iteration a value it owns,
/// given the current candidate set of owning loop variables?
pub(super) fn yields_owned(
    a: &HExpr,
    scope: &[(String, Origin)],
    cand: &HashSet<String>,
    bc: &BCheck,
) -> bool {
    match a {
        HExpr::Var(w) => match resolve(scope, w) {
            Some(Origin::LoopVar) => cand.contains(w),
            Some(Origin::Let { fresh }) => *fresh,
            Some(Origin::Binder { owner, heap }) => {
                *heap
                    && match owner {
                        BoxOwner::LoopVar(u) => cand.contains(u),
                        BoxOwner::Owned => true,
                        BoxOwner::Nobody => false,
                    }
            }
            None => false,
        },
        other => bc.allocates_fresh(other),
    }
}

/// Does every path through `e` end in a `recur`? Such a branch never
/// reaches the enclosing merge: control goes back to the loop head.
pub(super) fn always_recurs(e: &HExpr) -> bool {
    match e {
        HExpr::Recur(..) => true,
        HExpr::Let(_, body, _, _) => always_recurs(body),
        HExpr::Seq(vs) => vs.last().map(always_recurs).unwrap_or(false),
        HExpr::If(_, t, f, _) => always_recurs(t) && always_recurs(f),
        HExpr::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|a| always_recurs(&a.body))
        }
        _ => false,
    }
}

/// Bare variables handed to a tail `recur` of `e`: they leave the scope
/// as the next iteration's loop variables (the `recur` analogue of
/// `tail_escape`).
pub(super) fn recur_arg_vars(e: &HExpr, out: &mut HashSet<String>) {
    match e {
        HExpr::Recur(args, _) => {
            for a in args {
                if let HExpr::Var(v) = a {
                    out.insert(v.clone());
                }
            }
        }
        HExpr::Let(_, body, _, _) => recur_arg_vars(body, out),
        HExpr::Seq(vs) => {
            if let Some(last) = vs.last() {
                recur_arg_vars(last, out);
            }
        }
        HExpr::If(_, t, f, _) => {
            recur_arg_vars(t, out);
            recur_arg_vars(f, out);
        }
        HExpr::Match { arms, .. } => {
            for a in arms {
                recur_arg_vars(&a.body, out);
            }
        }
        _ => {}
    }
}

/// May a tail `recur` of `e` hand the next iteration something that points
/// into the memory of `alias` other than a bare alias variable? (A bare
/// binder is precise: its box slot is skipped and the loop variable takes
/// the object over. Anything else — a `&T` call over it, a `let` alias,
/// an element load — would dangle once the box is freed at the jump.)
pub(super) fn recur_args_may_alias(
    e: &HExpr,
    alias: &HashSet<String>,
    calls: &HashSet<String>,
) -> bool {
    match e {
        HExpr::Recur(args, _) => args.iter().any(|a| {
            !matches!(a, HExpr::Var(v) if alias.contains(v)) && tail_may_alias(a, alias, calls)
        }),
        HExpr::Let(binds, body, _, _) => {
            let mut inner = alias.clone();
            for (n, value, _) in binds {
                if tail_may_alias(value, &inner, calls) {
                    inner.insert(n.clone());
                }
            }
            recur_args_may_alias(body, &inner, calls)
        }
        HExpr::Seq(vs) => vs
            .last()
            .map(|l| recur_args_may_alias(l, alias, calls))
            .unwrap_or(false),
        HExpr::If(_, t, f, _) => {
            recur_args_may_alias(t, alias, calls) || recur_args_may_alias(f, alias, calls)
        }
        HExpr::Match { scrut, arms, .. } => {
            let scrut_alias = tail_may_alias(scrut, alias, calls);
            arms.iter().any(|arm| {
                let mut inner = alias.clone();
                if scrut_alias {
                    inner.extend(arm.binders.iter().cloned());
                }
                recur_args_may_alias(&arm.body, &inner, calls)
            })
        }
        _ => false,
    }
}

/// Is `name` bound anywhere inside `e` — by a `let`, a match arm, a loop,
/// a `for`/fused-loop stage or a closure parameter? The drop planner names
/// bindings by their source name, so a binding whose name is re-bound
/// inside its own scope is never a drop candidate: below the inner binding
/// (and, since the state maps are not restored after a `let` body, after
/// it too) the name may describe the other binding.
pub(super) fn binds_anywhere(e: &HExpr, name: &str) -> bool {
    let mut hit = false;
    crate::opt::walk::visit_nodes(e, &mut |n| match n {
        HExpr::Let(binds, _, _, _) => {
            if binds.iter().any(|(b, _, _)| b == name) {
                hit = true;
            }
        }
        HExpr::Loop { binds, .. } => {
            if binds.iter().any(|(b, _, _)| b == name) {
                hit = true;
            }
        }
        HExpr::Match { arms, .. } => {
            if arms.iter().any(|a| a.binders.iter().any(|b| b == name)) {
                hit = true;
            }
        }
        HExpr::For { el, .. } => {
            if el == name {
                hit = true;
            }
        }
        HExpr::Closure { params, .. } => {
            if params.iter().any(|(p, _)| p == name) {
                hit = true;
            }
        }
        HExpr::Reduce(r) => {
            let (maps, filters, extra): (&[MapSpec], &[FilterSpec], Vec<&String>) = match &**r {
                HReduce::Pipeline { maps, filters, .. } => (maps, filters, Vec::new()),
                HReduce::Closure {
                    maps,
                    filters,
                    acc,
                    el,
                    el2,
                    ..
                } => {
                    let mut v = vec![acc, el];
                    if let Some(e2) = el2 {
                        v.push(e2);
                    }
                    (maps, filters, v)
                }
            };
            if extra.iter().any(|b| *b == name)
                || maps
                    .iter()
                    .any(|m| m.el == name || m.el2.as_deref() == Some(name))
                || filters
                    .iter()
                    .any(|f| f.el == name || f.el2.as_deref() == Some(name))
            {
                hit = true;
            }
        }
        _ => {}
    });
    hit
}

impl<'a> BCheck<'a> {
    /// Which loop variables own their value on every iteration? A variable
    /// starts as a candidate when its seed handed it an owned value; it
    /// stays one only if every `recur` argument in its position yields an
    /// owned value too: a fresh allocation, an owning loop variable
    /// (itself, or a swap partner), a fresh `let` of this iteration, or a
    /// heap field moved out of a box this iteration owns (its slot is then
    /// skipped by the box's free). Anything else — an alias, a binding from
    /// outside the loop, a borrowed box's field — leaves the variable
    /// non-owning: never freed by the loop (a leak, never a double free).
    pub(super) fn loop_owning(
        &self,
        binds: &[(String, Ty, HExpr)],
        body: &HExpr,
        seed_owned: &HashSet<String>,
        tys: &HashMap<String, Ty>,
    ) -> HashSet<String> {
        let mut cand: HashSet<String> = binds
            .iter()
            .filter(|(n, t, _)| {
                heap_owning(*t, self.facts) && seed_owned.contains(n) && !binds_anywhere(body, n)
            })
            .map(|(n, _, _)| n.clone())
            .collect();
        if cand.is_empty() {
            return cand;
        }
        let names: Vec<String> = binds.iter().map(|(n, _, _)| n.clone()).collect();
        let mut tys2 = tys.clone();
        let mut scope: Vec<(String, Origin)> = Vec::new();
        for (n, t, _) in binds {
            tys2.insert(n.clone(), *t);
            scope.push((n.clone(), Origin::LoopVar));
        }
        let mut sites: Vec<RecurSite> = Vec::new();
        self.collect_recur_sites(body, &mut scope, &mut tys2, &mut sites);
        loop {
            let mut changed = false;
            for (args, sc) in &sites {
                for (i, a) in args.iter().enumerate() {
                    let Some(n) = names.get(i) else { continue };
                    if cand.contains(n) && !yields_owned(a, sc, &cand, self) {
                        cand.remove(n);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        cand
    }

    /// The tail `recur`s of one loop body (nested loops excluded — their
    /// recurs are their own), each with the bindings visible at it.
    pub(super) fn collect_recur_sites(
        &self,
        e: &HExpr,
        scope: &mut Vec<(String, Origin)>,
        tys: &mut HashMap<String, Ty>,
        sites: &mut Vec<RecurSite>,
    ) {
        match e {
            HExpr::Recur(args, _) => sites.push((args.clone(), scope.clone())),
            HExpr::Let(binds, body, _, _) => {
                let mark = scope.len();
                let saved: Vec<(String, Option<Ty>)> = binds
                    .iter()
                    .map(|(n, _, _)| (n.clone(), tys.get(n).copied()))
                    .collect();
                for (n, v, t) in binds {
                    let fresh = heap_owning(*t, self.facts) && self.allocates_fresh(v);
                    scope.push((n.clone(), Origin::Let { fresh }));
                    tys.insert(n.clone(), *t);
                }
                self.collect_recur_sites(body, scope, tys, sites);
                scope.truncate(mark);
                for (n, t) in saved.into_iter().rev() {
                    match t {
                        Some(t) => {
                            tys.insert(n, t);
                        }
                        None => {
                            tys.remove(&n);
                        }
                    }
                }
            }
            HExpr::Seq(vs) => {
                if let Some(l) = vs.last() {
                    self.collect_recur_sites(l, scope, tys, sites);
                }
            }
            HExpr::If(_, t, f, _) => {
                self.collect_recur_sites(t, scope, tys, sites);
                self.collect_recur_sites(f, scope, tys, sites);
            }
            HExpr::Match { scrut, arms, .. } => {
                // Who owns the matched box: an iteration binding (a loop
                // variable or a fresh `let`), or nobody but this match (a
                // fresh temp)? Its heap fields then pass ownership on.
                let owner = match &**scrut {
                    HExpr::Var(u) => match resolve(scope, u) {
                        Some(Origin::LoopVar) => BoxOwner::LoopVar(u.clone()),
                        Some(Origin::Let { fresh: true }) => BoxOwner::Owned,
                        _ => BoxOwner::Nobody,
                    },
                    other if fresh_box(other, self.ret_borrowed_names) => BoxOwner::Owned,
                    _ => BoxOwner::Nobody,
                };
                let adt = scrutinee_adt(scrut, tys, self.rets);
                for arm in arms {
                    let fields: Option<Vec<Ty>> = adt
                        .and_then(|i| self.adts.get(i as usize))
                        .and_then(|d| arm.variant.and_then(|v| d.variants.get(v as usize)))
                        .map(|f| f.1.clone());
                    let mark = scope.len();
                    for (k, b) in arm.binders.iter().enumerate() {
                        let fty = fields.as_ref().and_then(|f| f.get(k).copied());
                        let heap = binder_field_aliases(fty, &self.boxed_full)
                            && !matches!(fty, Some(Ty::Fn(_)));
                        scope.push((
                            b.clone(),
                            Origin::Binder {
                                owner: owner.clone(),
                                heap,
                            },
                        ));
                    }
                    self.collect_recur_sites(&arm.body, scope, tys, sites);
                    scope.truncate(mark);
                }
            }
            _ => {}
        }
    }

    /// Wrap each completing leaf of a loop body (every tail path that does
    /// not `recur`) in an empty-binds `Let` that drops `drops` after the
    /// leaf's value is computed — minus the variables that value may point
    /// into, and minus any name re-bound on the way down.
    pub(super) fn wrap_exit_leaves(
        &self,
        e: &mut HExpr,
        drops: &[String],
        extra: &HashMap<String, HashSet<String>>,
    ) {
        match e {
            HExpr::Recur(..) => {}
            HExpr::Let(binds, body, _, _) => {
                let mut ex = extra.clone();
                for (n, v, _) in binds.iter() {
                    let mut r = HashSet::new();
                    self.roots_into(v, &ex, &mut r);
                    ex.insert(n.clone(), r);
                }
                let keep: Vec<String> = drops
                    .iter()
                    .filter(|d| !binds.iter().any(|(n, _, _)| n == *d))
                    .cloned()
                    .collect();
                if !keep.is_empty() {
                    self.wrap_exit_leaves(body, &keep, &ex);
                }
            }
            HExpr::Seq(vs) => {
                if let Some(l) = vs.last_mut() {
                    self.wrap_exit_leaves(l, drops, extra);
                }
            }
            HExpr::If(_, t, f, _) => {
                self.wrap_exit_leaves(t, drops, extra);
                self.wrap_exit_leaves(f, drops, extra);
            }
            HExpr::Match { scrut, arms, .. } => {
                let mut r = HashSet::new();
                self.roots_into(scrut, extra, &mut r);
                for arm in arms.iter_mut() {
                    let mut ex = extra.clone();
                    for b in &arm.binders {
                        ex.insert(b.clone(), r.clone());
                    }
                    let keep: Vec<String> = drops
                        .iter()
                        .filter(|d| !arm.binders.contains(*d))
                        .cloned()
                        .collect();
                    if !keep.is_empty() {
                        self.wrap_exit_leaves(&mut arm.body, &keep, &ex);
                    }
                }
            }
            _ => {
                let mut r = HashSet::new();
                self.roots_into(e, extra, &mut r);
                let here: Vec<String> = drops.iter().filter(|d| !r.contains(*d)).cloned().collect();
                if !here.is_empty() {
                    let taken = std::mem::replace(e, HExpr::ConstI64(0));
                    *e = HExpr::Let(Vec::new(), Box::new(taken), here, Vec::new());
                }
            }
        }
    }

    /// A non-Copy value stored by pointer (a constructor field, an owned
    /// parameter) that is not a bare variable — an `if`/`match` merge, a
    /// `let`, a `do` — hands over every binding it may be: each tail leaf
    /// that is a non-Copy variable moves, and so do the non-Copy variables
    /// an alias-returning call leaf is given. A binder reaching a field
    /// through an `if` must count as moved, or its box's exit free would
    /// release it under the new holder.
    pub(super) fn move_value_roots(
        &mut self,
        a: &HExpr,
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        let mut leaves = Vec::new();
        tail_leaves(a, &mut leaves);
        // Direct references only: the field (or the variable) moves, never
        // the box it was read from — that box belongs to its match's exit
        // free and alias-escape rules (moving it would also retire the
        // scrutinee's other binders).
        let mut movers: Vec<String> = Vec::new();
        let non_copy = |v: &String, tys: &HashMap<String, Ty>| {
            tys.get(v).map(|t| is_non_copy(*t)).unwrap_or(false)
        };
        for l in leaves {
            match l {
                HExpr::Var(v) if non_copy(v, tys) => {
                    if !movers.contains(v) {
                        movers.push(v.clone());
                    }
                }
                // An alias-returning call over a variable hands that variable
                // over with its result.
                HExpr::Call(f, cargs)
                    if self.alias_calls.contains(f)
                        && self.rets.get(f).map(|t| is_non_copy(*t)).unwrap_or(false) =>
                {
                    for ca in cargs {
                        if let HExpr::Var(v) = ca {
                            if non_copy(v, tys) && !movers.contains(v) {
                                movers.push(v.clone());
                            }
                        }
                    }
                }
                // Constructors move their own arguments when checked; calls
                // to keeping callees move theirs; everything else is fresh
                // or a copy.
                _ => {}
            }
        }
        for r in movers {
            if st.get(&r) != Some(&VState::Moved) && !self.no_move.contains(&r) {
                self.move_var(&r, st, owns);
            }
        }
    }

    /// Move `v` into the next iteration (a bare `recur` argument). Like
    /// `move_var`, except that the duty has a receiver inside this very
    /// scope: it is not "consumed" (a completing path that never took
    /// this `recur` still frees `v`).
    pub(super) fn hand_over(
        &mut self,
        v: &str,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        let was = self.consumed.contains(v);
        self.move_var(v, st, owns);
        if !was {
            self.consumed.remove(v);
        }
    }
}
