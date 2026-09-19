use super::*;
use std::collections::{HashMap, HashSet};

pub(crate) fn is_non_copy(ty: Ty) -> bool {
    // Closures are move-only owning values (M3): the header uniquely names
    // the env, whose drop glue is invoked at the owner's scope exit.
    matches!(ty, Ty::Adt(_) | Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Fn(_))
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum VState {
    Owned,
    Moved,
}

/// One traversal does three jobs: move checking, per-`Let` drop-list
/// planning, and (optionally) the param-free wrapper. The two maps carried
/// beside the state machine:
///
/// - `owns[b]` — does binding `b` currently hold a heap allocation it is
///   responsible for? Moves and consumes clear it (the duty transfers);
///   allocating initializers set it. A binding frees at scope exit iff it
///   still `owns` and was never moved.
/// - `consumed[b]` — the drop duty for `b`'s buffer left this function (or
///   moved into a fresh binding of the same name), so no scope may emit a
///   free for it.
pub(crate) struct BCheck<'a> {
    /// callee name -> per-param borrow kinds (owned by default)
    pub(crate) sigs: &'a HashMap<String, Vec<Borrow>>,
    /// Parameters the function does not own (`&`): they still belong
    /// to the caller, so moving them anywhere in the body is an error.
    pub(crate) no_move: HashSet<String>,
    /// Representation facts (boxing/ownership) for ADT eligibility.
    pub(crate) facts: &'a AdtFacts,
    /// Names in tail (returned) position: the caller inherits their drops.
    pub(crate) escaped: HashSet<String>,
    /// Names bound more than once in the defn: codegen shares the env slot,
    /// so frees stay suppressed (known limitation).
    pub(crate) shadowed: HashSet<String>,
    /// Consumed names (see struct docs).
    pub(crate) consumed: HashSet<String>,
    /// Remaining occurrences of each name (pre-counted); a conj source whose
    /// count hits 0 at the conj is a last use, so in-place update is
    /// unobservable.
    pub(crate) remaining: HashMap<String, i64>,
    /// Params whose buffers free at body exit (result, when requested).
    pub(crate) param_frees: Vec<String>,
    pub(crate) errs: Vec<String>,
}

/// Check move semantics and plan drops for one body. With
/// `with_param_frees`, owned move-type parameters that still hold their
/// buffer at body exit get a scope-exit free (an empty-binds `Let` wrapper,
/// which the backends already emit).
pub(crate) fn plan_drops(
    name: &str,
    params: &[(String, Ty)],
    borrows: &[Borrow],
    body: &mut HExpr,
    sigs: &HashMap<String, Vec<Borrow>>,
    facts: &AdtFacts,
    with_param_frees: bool,
) -> Result<(), String> {
    let mut escaped = HashSet::new();
    tail_escape(body, &mut escaped);

    // Shadowing pre-pass: names bound by more than one `let` share a codegen
    // slot, so they never free.
    let mut counts: HashMap<String, usize> = HashMap::new();
    count_binds(body, &mut counts);
    let shadowed: HashSet<String> = counts
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(n, _)| n)
        .collect();

    let mut var_counts: HashMap<String, i64> = HashMap::new();
    count_vars(body, &mut var_counts);
    let mut bc = BCheck {
        sigs,
        no_move: params
            .iter()
            .zip(borrows.iter())
            .filter(|(_, b)| !b.is_owned())
            .map(|((n, _), _)| n.clone())
            .collect(),
        facts,
        escaped,
        shadowed,
        consumed: HashSet::new(),
        remaining: var_counts,
        param_frees: Vec::new(),
        errs: Vec::new(),
    };
    let mut tys: HashMap<String, Ty> = HashMap::new();
    let mut st: HashMap<String, VState> = HashMap::new();
    let mut owns: HashMap<String, bool> = HashMap::new();
    for ((n, t), b) in params.iter().zip(borrows.iter()) {
        tys.insert(n.clone(), *t);
        // Params taken by value start owned; borrowed params stay with the
        // caller (and are still usable here).
        st.insert(n.clone(), VState::Owned);
        let moves_by_value = param_moves_by_value(*t, facts);
        owns.insert(n.clone(), b.is_owned() && moves_by_value);
    }
    bc.check(body, &mut tys, &mut st, &mut owns);

    if with_param_frees {
        // Owned move-type parameters whose buffer is still here free at body
        // exit. A moved param (consumed) or a returned one does not: its
        // destruction duty transferred. Inline (non-boxed) owning ADTs are
        // excluded — by-value copies alias their field buffers.
        for (k, (n, ty)) in params.iter().enumerate() {
            let moves = param_moves_by_value(*ty, facts);
            if moves
                && owns.get(n).copied().unwrap_or(false)
                && st.get(n) == Some(&VState::Owned)
                && !bc.escaped.contains(n)
                && borrows.get(k).copied().unwrap_or(Borrow::Owned).is_owned()
            {
                bc.param_frees.push(n.clone());
            }
        }
        if !bc.param_frees.is_empty() {
            let taken = std::mem::replace(body, HExpr::ConstI64(0));
            *body = HExpr::Let(Vec::new(), Box::new(taken), bc.param_frees);
        }
    }

    if bc.errs.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "ownership check failed in {}: {}",
            name,
            bc.errs.join("; ")
        ))
    }
}

/// Params of these types carry a uniquely-owned buffer when passed by value.
/// Boxed ADTs move by pointer (always heap); closures move their env.
/// Inline owning ADTs copy their bytes (field buffers alias across the
/// copy), and collection headers {ptr,len,cap} cannot carry provenance —
/// the pointer may be static data (a literal) — so those never free at
/// param scope.
fn param_moves_by_value(ty: Ty, facts: &AdtFacts) -> bool {
    match ty {
        Ty::Fn(_) => true,
        Ty::Adt(i) => facts.boxed.get(i as usize).copied().unwrap_or(false),
        _ => false,
    }
}

fn count_binds(e: &HExpr, counts: &mut HashMap<String, usize>) {
    match e {
        HExpr::Let(binds, body, _) => {
            for (n, value, _) in binds {
                *counts.entry(n.clone()).or_insert(0) += 1;
                count_binds(value, counts);
            }
            count_binds(body, counts);
        }
        HExpr::Seq(vs) | HExpr::StrCat(vs) => vs.iter().for_each(|v| count_binds(v, counts)),
        HExpr::If(c, t, f, _) => {
            count_binds(c, counts);
            count_binds(t, counts);
            count_binds(f, counts);
        }
        HExpr::Loop { binds, body, .. } => {
            binds.iter().for_each(|(_, _, e)| count_binds(e, counts));
            count_binds(body, counts);
        }
        HExpr::Recur(args) => args.iter().for_each(|a| count_binds(a, counts)),
        HExpr::ToStr(e, _) => count_binds(e, counts),
        HExpr::ParseNum { s, .. } => count_binds(s, counts),
        HExpr::Slurp(p) => count_binds(p, counts),
        HExpr::Spit { path, data } => {
            count_binds(path, counts);
            count_binds(data, counts);
        }
        HExpr::Exit(c) => count_binds(c, counts),
        HExpr::And(c, t) | HExpr::Or(c, t) => {
            count_binds(c, counts);
            count_binds(t, counts);
        }
        _ => {}
    }
}

/// Vars referenced in tail position of a body (the returned value chain).
fn tail_escape(e: &HExpr, escaped: &mut HashSet<String>) {
    match e {
        HExpr::Let(_, body, _) => tail_escape(body, escaped),
        HExpr::Seq(vs) => {
            if let Some(last) = vs.last() {
                tail_escape(last, escaped);
            }
        }
        // Branch results are tail positions too: a binding returned from
        // an `if`/`match` arm transfers its drop duty to the result.
        HExpr::If(_, then, els, _) => {
            tail_escape(then, escaped);
            tail_escape(els, escaped);
        }
        HExpr::Match { arms, .. } => {
            for arm in arms {
                tail_escape(&arm.body, escaped);
            }
        }
        HExpr::Var(v) => {
            escaped.insert(v.clone());
        }
        _ => {}
    }
}

impl<'a> BCheck<'a> {
    /// Borrow kinds of a callee; builtins never consume.
    pub(crate) fn kinds_of(&self, name: &str, arity: usize) -> Vec<Borrow> {
        if let Some(b) = self.sigs.get(name) {
            return b.clone();
        }
        match name {
            "count" | "even?" | "print-i64" | "print-f64" | "get" => {
                vec![Borrow::Shared; arity]
            }
            "clone" | "drop" => vec![Borrow::Shared; arity],
            _ => vec![Borrow::Owned; arity],
        }
    }

    pub(crate) fn use_var(
        &mut self,
        v: &str,
        tys: &HashMap<String, Ty>,
        st: &HashMap<String, VState>,
    ) {
        if let Some(r) = self.remaining.get_mut(v) {
            *r -= 1;
        }
        if let Some(ty) = tys.get(v) {
            if is_non_copy(*ty) && st.get(v) == Some(&VState::Moved) {
                self.errs.push(format!("use of moved value {}", v));
            }
        }
    }

    /// Move binding `v`: its state goes Moved and its drop duty leaves it.
    /// Borrowed parameters cannot move — the caller still owns them, and an
    /// owned callee would free the shared buffer.
    pub(crate) fn move_var(
        &mut self,
        v: &str,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        if self.no_move.contains(v) {
            self.errs.push(format!(
                "cannot move {}: it is a borrowed parameter and still belongs to the caller",
                v
            ));
            return;
        }
        st.insert(v.to_string(), VState::Moved);
        owns.insert(v.to_string(), false);
        self.consumed.insert(v.to_string());
    }

    /// Move a Var source if it names a non-Copy binding (the move sites:
    /// call arguments, conj sources, closure captures, drop, let-of-var).
    fn move_if_non_copy(
        &mut self,
        v: &str,
        tys: &HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        if tys.get(v).map(|t| is_non_copy(*t)).unwrap_or(false) {
            self.use_var(v, tys, st);
            self.move_var(v, st, owns);
        }
    }

    pub(crate) fn check(
        &mut self,
        e: &mut HExpr,
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        match e {
            HExpr::Var(v) => self.use_var(v, tys, st),
            HExpr::StrLit(_) | HExpr::StrCmp(..) | HExpr::StrOrd(..) | HExpr::ConstB(..)
            | HExpr::CharStr(..) => {}
            HExpr::StrNth { s, idx, .. } => {
                self.check(s, tys, st, owns);
                self.check(idx, tys, st, owns);
            }
            // Loop variables are Copy in this slice: no moves, no frees.
            // Recur arguments are non-moving reads of Copy values, so the
            // once-per-body check stays sound across iterations.
            HExpr::Loop { binds, body, .. } => {
                let mut shadowed_vars: Vec<(String, Option<VState>)> = Vec::new();
                for (n, ty, init) in binds.iter_mut() {
                    self.check(init, tys, st, owns);
                    shadowed_vars.push((n.clone(), st.get(n).copied()));
                    tys.insert(n.clone(), *ty);
                    st.insert(n.clone(), VState::Owned);
                }
                self.check(body, tys, st, owns);
                for (n, prev) in shadowed_vars.iter().rev() {
                    match prev {
                        Some(v) => {
                            st.insert(n.clone(), *v);
                        }
                        None => {
                            tys.remove(n);
                            st.remove(n);
                        }
                    }
                }
            }
            HExpr::Recur(args) => {
                for a in args {
                    self.check(a, tys, st, owns);
                }
            }
            HExpr::ToStr(e, _) => self.check(e, tys, st, owns),
            HExpr::ParseNum { s, .. } => self.check(s, tys, st, owns),
            HExpr::Slurp(p) => self.check(p, tys, st, owns),
            HExpr::Spit { path, data } => {
                self.check(path, tys, st, owns);
                self.check(data, tys, st, owns);
            }
            HExpr::Args => {}
            HExpr::Exit(c) => self.check(c, tys, st, owns),
            HExpr::StrCat(args) => {
                for a in args {
                    self.check(a, tys, st, owns);
                }
            }
            HExpr::BinW(_, a, b, _) => {
                self.check(a, tys, st, owns);
                self.check(b, tys, st, owns);
            }
            HExpr::CmpW(_, a, b, _) | HExpr::BinF(_, a, b) => {
                self.check(a, tys, st, owns);
                self.check(b, tys, st, owns);
            }
            HExpr::ICast(inner, _, _) => self.check(inner, tys, st, owns),
            HExpr::MapLit(pairs) => {
                for (k, v) in pairs {
                    self.check(k, tys, st, owns);
                    self.check(v, tys, st, owns);
                }
            }
            HExpr::MapAssoc { m, k, v } => {
                self.check(m, tys, st, owns);
                self.check(k, tys, st, owns);
                self.check(v, tys, st, owns);
            }
            HExpr::MapGet { src, key, default, .. } => {
                self.check(src, tys, st, owns);
                self.check(key, tys, st, owns);
                if let Some(d) = default {
                    self.check(d, tys, st, owns);
                }
            }
            // Sets are immutable snapshots like maps: reading them borrows.
            HExpr::SetLit(vs) => {
                for v in vs {
                    self.check(v, tys, st, owns);
                }
            }
            HExpr::SetConj { s, x }
            | HExpr::SetDisj { s, x }
            | HExpr::SetContains { s, x } => {
                self.check(s, tys, st, owns);
                self.check(x, tys, st, owns);
            }
            HExpr::Subs { s, start, end } => {
                self.check(s, tys, st, owns);
                self.check(start, tys, st, owns);
                self.check(end, tys, st, owns);
            }
            HExpr::Split { s, sep } => {
                self.check(s, tys, st, owns);
                self.check(sep, tys, st, owns);
            }
            HExpr::Join { sep, xs } => {
                self.check(sep, tys, st, owns);
                self.check(xs, tys, st, owns);
            }
            HExpr::Subvec { v, start, end, .. } => {
                self.check(v, tys, st, owns);
                self.check(start, tys, st, owns);
                self.check(end, tys, st, owns);
            }
            HExpr::For { .. } => {} // per-iteration: captures are borrows
            HExpr::Conj {
                v,
                x,
                in_place,
                ..
            } => {
                self.check(v, tys, st, owns);
                // Persistent semantics: the source stays valid. In-place
                // update is chosen only when the source is a last use of a
                // binding that really owns a heap buffer — then the append
                // is unobservable and the drop duty transfers to the result.
                let mut last_use_owned = false;
                if let HExpr::Var(name) = &**v {
                    let name = name.clone();
                    last_use_owned = tys.get(&name).map(|t| is_non_copy(*t)).unwrap_or(false)
                        && st.get(&name) == Some(&VState::Owned)
                        && owns.get(&name).copied().unwrap_or(false)
                        && self.remaining.get(&name).copied() == Some(0);
                    if last_use_owned {
                        self.move_var(&name, st, owns);
                    }
                }
                *in_place = last_use_owned;
                self.check(x, tys, st, owns);
            }
            HExpr::Clone(inner) => self.check(inner, tys, st, owns),
            // Creating a closure captures free locals by value: non-Copy
            // captures move, and each capture's heap ownership (or lack of
            // it — static literals, aliases) is recorded in the node so the
            // env's drop glue frees only what the env really owns.
            HExpr::Closure {
                captures,
                capture_owns,
                ..
            } => {
                for ((name, ty), own_bit) in captures.iter_mut().zip(capture_owns.iter_mut()) {
                    self.use_var(name, tys, st);
                    *own_bit = owns.get(name).copied().unwrap_or(false);
                    if is_non_copy(*ty) {
                        self.move_var(name, st, owns);
                    }
                }
            }
            HExpr::ClosureCall { f, args, .. } => {
                // Calling a closure reads the binding (params get a local
                // copy of the closure value).
                self.check(f, tys, st, owns);
                for a in args {
                    self.check(a, tys, st, owns);
                }
            }
            HExpr::DropStmt(inner) => {
                self.check(inner, tys, st, owns);
                if let HExpr::Var(v) = &**inner {
                    let v = v.clone();
                    self.move_if_non_copy(&v, tys, st, owns);
                }
            }
            HExpr::And(a, b) | HExpr::Or(a, b) => {
                self.check(a, tys, st, owns);
                self.check(b, tys, st, owns);
            }
            HExpr::Not(v) | HExpr::Abs(v, _) => self.check(v, tys, st, owns),
            HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
                self.check(a, tys, st, owns);
                self.check(b, tys, st, owns);
            }
            HExpr::Count(v) => self.check(v, tys, st, owns),
            HExpr::Call(name, args) => {
                for a in args.iter_mut() {
                    self.check(a, tys, st, owns);
                }
                let kinds = self.kinds_of(name, args.len());
                for (arg, kind) in args.iter().zip(kinds.iter()) {
                    if let HExpr::Var(v) = arg {
                        let non_copy = tys.get(v).map(|t| is_non_copy(*t)).unwrap_or(false);
                        if !non_copy {
                            continue;
                        }
                        if *kind == Borrow::Owned {
                            let v = v.clone();
                            self.use_var(&v, tys, st);
                            self.move_var(&v, st, owns);
                        }
                    }
                }
            }
            HExpr::Ctor { adt, args, .. } => {
                // Boxed or inline, an owning ADT's field slot is freed by
                // the box's drop glue: a Var argument transfers its drop
                // duty to the constructor (use-after-ctor is a move error).
                let owns_box = self.facts.owns.get(*adt as usize).copied().unwrap_or(false);
                for a in args {
                    self.check(a, tys, st, owns);
                    if owns_box {
                        if let HExpr::Var(v) = a {
                            self.move_if_non_copy(v, tys, st, owns);
                        }
                    }
                }
            }
            HExpr::Match { scrut, arms, .. } => {
                self.check(scrut, tys, st, owns);
                // Every arm starts from the same post-scrutinee state: a
                // move in one arm (e.g. a generic-dispatch call) must not
                // poison the other arms — only one arm runs.
                let post_scrut = st.clone();
                let post_owns = owns.clone();
                let post_tys = tys.clone();
                for arm in arms.iter_mut() {
                    let mut arm_tys = post_tys.clone();
                    let mut arm_st = post_scrut.clone();
                    let mut arm_owns = post_owns.clone();
                    for b in &arm.binders {
                        arm_tys.insert(b.clone(), Ty::I64);
                        arm_st.insert(b.clone(), VState::Owned);
                        arm_owns.insert(b.clone(), false);
                    }
                    // arm binder types: recover field types are not carried;
                    // binders alias the scrutinee's fields, so they never
                    // own (I64 is the historical state-tracking stand-in).
                    self.check(&mut arm.body, &mut arm_tys, &mut arm_st, &mut arm_owns);
                    // A value moved or consumed in any arm counts as moved
                    // and unconsumed-eligible afterwards.
                    for (k, v) in arm_st.iter() {
                        if *v == VState::Moved {
                            st.insert(k.clone(), *v);
                        }
                    }
                    for (k, v) in arm_owns.iter() {
                        if !*v {
                            owns.insert(k.clone(), false);
                        }
                    }
                }
            }
            HExpr::Get { src, idx, default, .. } => {
                self.check(src, tys, st, owns);
                self.check(idx, tys, st, owns);
                if let Some(d) = default {
                    self.check(d, tys, st, owns);
                }
            }
            HExpr::If(c, t, e2, _) => {
                self.check(c, tys, st, owns);
                let mut t_tys = tys.clone();
                let mut t_st = st.clone();
                let mut t_owns = owns.clone();
                self.check(t, &mut t_tys, &mut t_st, &mut t_owns);
                let mut e_tys = tys.clone();
                let mut e_st = st.clone();
                let mut e_owns = owns.clone();
                self.check(e2, &mut e_tys, &mut e_st, &mut e_owns);
                for (k, v) in t_st.iter().chain(e_st.iter()) {
                    if *v == VState::Moved {
                        st.insert(k.clone(), *v);
                    }
                }
                for (k, v) in t_owns.iter().chain(e_owns.iter()) {
                    if !*v {
                        owns.insert(k.clone(), false);
                    }
                }
            }
            HExpr::Let(binds, body, frees) => {
                for (name, value, ty) in binds.iter_mut() {
                    self.check(value, tys, st, owns);
                    // Binding a non-Copy Var moves the buffer: the drop
                    // duty transfers to the new binding. Read the source's
                    // ownership before the move clears it.
                    let transferred = if let HExpr::Var(v) = &*value {
                        if is_non_copy(*ty) {
                            let src_owns = owns.get(v).copied().unwrap_or(false);
                            self.move_var(v, st, owns);
                            Some(src_owns)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    // A fresh binding starts unconsumed (shadowing a
                    // consumed outer name does not inherit the flag).
                    self.consumed.remove(name);
                    owns.insert(
                        name.clone(),
                        match transferred {
                            // Non-Copy Var: the move above decided it.
                            Some(o) => o,
                            // Copy alias (str from a Var): never owns.
                            None if matches!(value, HExpr::Var(_)) => false,
                            None => allocates(value, self.facts),
                        },
                    );
                    tys.insert(name.clone(), *ty);
                    st.insert(name.clone(), VState::Owned);
                }
                self.check(body, tys, st, owns);
                // Scope exit: every binding that still owns a heap buffer
                // and was not moved, consumed, returned, or shadowed frees
                // here. This is the whole destruction rule.
                let mut out = Vec::new();
                for (name, _, ty) in binds.iter() {
                    if heap_owning(*ty, self.facts)
                        && owns.get(name).copied().unwrap_or(false)
                        && st.get(name) == Some(&VState::Owned)
                        && !self.escaped.contains(name)
                        && !self.consumed.contains(name)
                        && !self.shadowed.contains(name)
                    {
                        out.push(name.clone());
                    }
                }
                *frees = out;
            }
            HExpr::Seq(vs) => {
                for v in vs {
                    self.check(v, tys, st, owns);
                }
            }
            // Fused loop stages run per iteration: captures are shared
            // (borrowed), never moved.
            HExpr::Reduce(_) => {}
            HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::VecLit(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Ownership-driven destruction (the drop planner)
// ---------------------------------------------------------------------------

/// Total occurrences of every name in the body (exact, all positions — a
/// missed occurrence would make in-place update unsound, so this walks
/// everything, including fused-loop bodies and closure captures).
fn count_vars(e: &HExpr, counts: &mut HashMap<String, i64>) {
    let mut bump = |name: &str| *counts.entry(name.to_string()).or_insert(0) += 1;
    match e {
        HExpr::Var(v) => bump(v),
        HExpr::StrNth { s, idx, .. } => {
            count_vars(s, counts);
            count_vars(idx, counts);
        }
        HExpr::Loop { binds, body, .. } => {
            binds.iter().for_each(|(_, _, i)| count_vars(i, counts));
            count_vars(body, counts);
        }
        HExpr::Recur(args) => args.iter().for_each(|a| count_vars(a, counts)),
        HExpr::ToStr(e2, _) => count_vars(e2, counts),
        HExpr::ParseNum { s, .. } => count_vars(s, counts),
        HExpr::Slurp(p) => count_vars(p, counts),
        HExpr::Spit { path, data } => {
            count_vars(path, counts);
            count_vars(data, counts);
        }
        HExpr::Exit(c) => count_vars(c, counts),
        HExpr::StrCat(args) => args.iter().for_each(|a| count_vars(a, counts)),
        HExpr::BinW(_, a, b, _) | HExpr::CmpW(_, a, b, _) | HExpr::BinF(_, a, b) => {
            count_vars(a, counts);
            count_vars(b, counts);
        }
        HExpr::ICast(inner, _, _) => count_vars(inner, counts),
        HExpr::MapLit(pairs) => pairs.iter().for_each(|(k, v)| {
            count_vars(k, counts);
            count_vars(v, counts);
        }),
        HExpr::MapAssoc { m, k, v } => {
            count_vars(m, counts);
            count_vars(k, counts);
            count_vars(v, counts);
        }
        HExpr::MapGet { src, key, default, .. } => {
            count_vars(src, counts);
            count_vars(key, counts);
            default.as_ref().map(|d| count_vars(d, counts));
        }
        HExpr::SetLit(vs) => vs.iter().for_each(|v| count_vars(v, counts)),
        HExpr::SetConj { s, x } | HExpr::SetDisj { s, x } | HExpr::SetContains { s, x } => {
            count_vars(s, counts);
            count_vars(x, counts);
        }
        HExpr::Subs { s, start, end } => {
            count_vars(s, counts);
            count_vars(start, counts);
            count_vars(end, counts);
        }
        HExpr::Split { s, sep } => {
            count_vars(s, counts);
            count_vars(sep, counts);
        }
        HExpr::Join { sep, xs } => {
            count_vars(sep, counts);
            count_vars(xs, counts);
        }
        HExpr::Subvec { v, start, end, .. } => {
            count_vars(v, counts);
            count_vars(start, counts);
            count_vars(end, counts);
        }
        HExpr::For { src, body, .. } => {
            count_vars(src, counts);
            count_vars(body, counts);
        }
        HExpr::Conj { v, x, .. } => {
            count_vars(v, counts);
            count_vars(x, counts);
        }
        HExpr::Clone(inner) => count_vars(inner, counts),
        HExpr::Closure { captures, body, .. } => {
            captures.iter().for_each(|(n, _)| bump(n));
            count_vars(body, counts);
        }
        HExpr::ClosureCall { f, args, .. } => {
            count_vars(f, counts);
            args.iter().for_each(|a| count_vars(a, counts));
        }
        HExpr::DropStmt(inner) => count_vars(inner, counts),
        HExpr::And(a, b) | HExpr::Or(a, b) => {
            count_vars(a, counts);
            count_vars(b, counts);
        }
        HExpr::Not(v) | HExpr::Abs(v, _) => count_vars(v, counts),
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
            count_vars(a, counts);
            count_vars(b, counts);
        }
        HExpr::Count(v) => count_vars(v, counts),
        HExpr::Call(_, args) => args.iter().for_each(|a| count_vars(a, counts)),
        HExpr::Ctor { args, .. } => args.iter().for_each(|a| count_vars(a, counts)),
        HExpr::Match { scrut, arms, .. } => {
            count_vars(scrut, counts);
            arms.iter().for_each(|a| count_vars(&a.body, counts));
        }
        HExpr::Get { src, idx, default, .. } => {
            count_vars(src, counts);
            count_vars(idx, counts);
            default.as_ref().map(|d| count_vars(d, counts));
        }
        HExpr::If(c, t, f, _) => {
            count_vars(c, counts);
            count_vars(t, counts);
            count_vars(f, counts);
        }
        HExpr::Let(binds, body, _) => {
            binds.iter().for_each(|(_, v, _)| count_vars(v, counts));
            count_vars(body, counts);
        }
        HExpr::Seq(vs) => vs.iter().for_each(|v| count_vars(v, counts)),
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline { init, src, .. } => {
                count_vars(init, counts);
                count_source(src, counts);
            }
            HReduce::Closure { init, src, body, maps, filters, .. } => {
                count_vars(init, counts);
                count_source(src, counts);
                count_vars(body, counts);
                maps.iter().for_each(|m| count_vars(&m.body, counts));
                filters.iter().for_each(|f| count_vars(&f.body, counts));
            }
        },
        HExpr::StrCmp(_, a, b) | HExpr::StrOrd(_, a, b) => {
            count_vars(a, counts);
            count_vars(b, counts);
        }
        HExpr::CharStr(c) => count_vars(c, counts),
        HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::ConstB(_) | HExpr::StrLit(_)
        | HExpr::VecLit(_) | HExpr::Args => {}
    }
}

fn count_source(src: &HSource, counts: &mut HashMap<String, i64>) {
    match src {
        HSource::Var(v) => *counts.entry(v.clone()).or_insert(0) += 1,
        HSource::Zip(a, b) => {
            count_source(a, counts);
            count_source(b, counts);
        }
        HSource::Take(n, inner) | HSource::Drop(n, inner) => {
            count_vars(n, counts);
            count_source(inner, counts);
        }
        HSource::Range(hi, _) => count_vars(hi, counts),
        HSource::Lit(_) => {}
    }
}

/// Heap-owning types: bindings of these types hold a malloc'd buffer that
/// can be released at binding scope exit. Strings and owning ADTs joined
/// the model in S1c.
fn heap_owning(ty: Ty, facts: &AdtFacts) -> bool {
    match ty {
        Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Str | Ty::Fn(_) => true,
        Ty::Adt(i) => facts.owns.get(i as usize).copied().unwrap_or(false),
        _ => false,
    }
}

/// Does this value expression allocate a fresh heap buffer that the binding
/// uniquely owns? Aliases (`Var`), element loads (`Get`), and control-flow
/// results are excluded — control-flow merges leak as today until per-arm
/// scratch drops exist. Compared to the previous shape heuristic this adds
/// the allocating forms that were missed: `clone`, `ToStr`, `slurp`,
/// `parse-*`, `args`, and inline-owning constructors.
fn allocates(value: &HExpr, facts: &AdtFacts) -> bool {
    match value {
        // Fresh heap strings.
        HExpr::Subs { .. }
        | HExpr::Split { .. }
        | HExpr::Join { .. }
        | HExpr::StrCat(_)
        | HExpr::CharStr(_)
        | HExpr::ToStr(_, _) => true,
        // Fresh collections.
        HExpr::Conj { .. }
        | HExpr::Subvec { .. }
        | HExpr::For { .. }
        | HExpr::MapLit(_)
        | HExpr::MapAssoc { .. }
        | HExpr::SetLit(_)
        | HExpr::SetConj { .. }
        | HExpr::SetDisj { .. }
        | HExpr::Args => true,
        // A fresh box (or fresh inline-owning fields) for an ADT; a fresh
        // collection from a call; an explicit deep copy.
        HExpr::Ctor { adt, .. } => facts.owns.get(*adt as usize).copied().unwrap_or(false),
        HExpr::ParseNum { res, .. } => matches!(res, Ty::Adt(i) if facts.owns.get(*i as usize).copied().unwrap_or(false)),
        // A closure mints its owning env allocation.
        HExpr::Closure { .. } => true,
        HExpr::Call(_, _) | HExpr::Clone(_) | HExpr::Slurp(_) => true,
        _ => false,
    }
}
