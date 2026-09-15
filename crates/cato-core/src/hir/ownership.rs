use super::*;
use crate::Program;
use std::collections::{HashMap, HashSet};

pub(crate) fn is_non_copy(ty: Ty) -> bool {
    // Closures stay Copy: their env allocation lives until process exit
    // (no frees), so header copies aliasing it are observationally sound.
    matches!(ty, Ty::Adt(_) | Ty::Vec(_) | Ty::Map | Ty::Set)
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum VState {
    Owned,
    Moved,
}

pub(crate) struct BCheck<'a> {
    /// callee name -> per-param borrow kinds (owned by default)
    pub(crate) sigs: &'a HashMap<String, Vec<Borrow>>,
    pub(crate) errs: Vec<String>,
}

pub(crate) fn check_ownership(defn: &Defn, sigs: &HashMap<String, Vec<Borrow>>) -> Result<(), String> {
    let mut bc = BCheck {
        sigs,
        errs: Vec::new(),
    };
    let mut tys: HashMap<String, Ty> = HashMap::new();
    let mut st: HashMap<String, VState> = HashMap::new();
    for ((n, t), b) in defn.params.iter().zip(defn.borrows.iter()) {
        tys.insert(n.clone(), *t);
        // Params taken by value start owned; borrowed params stay with the
        // caller (and are still usable here).
        st.insert(n.clone(), VState::Owned);
        let _ = b;
    }
    bc.check(&defn.body, &mut tys, &mut st);
    if bc.errs.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "ownership check failed in {}: {}",
            defn.name,
            bc.errs.join("; ")
        ))
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

    pub(crate) fn use_var(&mut self, v: &str, tys: &HashMap<String, Ty>, st: &HashMap<String, VState>) {
        if let Some(ty) = tys.get(v) {
            if is_non_copy(*ty) && st.get(v) == Some(&VState::Moved) {
                self.errs.push(format!("use of moved value {}", v));
            }
        }
    }

    pub(crate) fn check(
        &mut self,
        e: &HExpr,
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
    ) {
        match e {
            HExpr::Var(v) => self.use_var(v, tys, st),
            HExpr::StrLit(_) | HExpr::StrCmp(..) | HExpr::StrOrd(..) | HExpr::ConstB(..)
            | HExpr::CharStr(..) => {}
            HExpr::StrNth { s, idx } => {
                self.check(s, tys, st);
                self.check(idx, tys, st);
            }
            // Loop variables are Copy in this slice: no moves, no frees.
            // Recur arguments are non-moving reads of Copy values, so the
            // once-per-body check stays sound across iterations.
            HExpr::Loop { binds, body, .. } => {
                let mut shadowed_vars: Vec<(String, Option<VState>)> = Vec::new();
                for (n, ty, init) in binds.iter() {
                    self.check(init, tys, st);
                    shadowed_vars.push((n.clone(), st.get(n).copied()));
                    tys.insert(n.clone(), *ty);
                    st.insert(n.clone(), VState::Owned);
                }
                self.check(body, tys, st);
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
                    self.check(a, tys, st);
                }
            }
            HExpr::ToStr(e, _) => self.check(e, tys, st),
            HExpr::ParseNum { s, .. } => self.check(s, tys, st),
            HExpr::Slurp(p) => self.check(p, tys, st),
            HExpr::Spit { path, data } => {
                self.check(path, tys, st);
                self.check(data, tys, st);
            }
            HExpr::Args => {}
            HExpr::Exit(c) => self.check(c, tys, st),
            HExpr::StrCat(args) => {
                for a in args {
                    self.check(a, tys, st);
                }
            }
            HExpr::BinW(_, a, b, _) => {
                self.check(a, tys, st);
                self.check(b, tys, st);
            }
            HExpr::CmpW(_, a, b, _) | HExpr::BinF(_, a, b) => {
                self.check(a, tys, st);
                self.check(b, tys, st);
            }
            HExpr::ICast(inner, _) => self.check(inner, tys, st),
            HExpr::MapLit(pairs) => {
                for (k, v) in pairs {
                    self.check(k, tys, st);
                    self.check(v, tys, st);
                }
            }
            HExpr::MapAssoc { m, k, v } => {
                self.check(m, tys, st);
                self.check(k, tys, st);
                self.check(v, tys, st);
            }
            HExpr::MapGet { src, key, default } => {
                self.check(src, tys, st);
                self.check(key, tys, st);
                if let Some(d) = default {
                    self.check(d, tys, st);
                }
            }
            // Sets are immutable snapshots like maps: reading them borrows.
            HExpr::SetLit(vs) => {
                for v in vs {
                    self.check(v, tys, st);
                }
            }
            HExpr::SetConj { s, x }
            | HExpr::SetDisj { s, x }
            | HExpr::SetContains { s, x } => {
                self.check(s, tys, st);
                self.check(x, tys, st);
            }
            HExpr::Subs { s, start, end } => {
                self.check(s, tys, st);
                self.check(start, tys, st);
                self.check(end, tys, st);
            }
            HExpr::Split { s, sep } => {
                self.check(s, tys, st);
                self.check(sep, tys, st);
            }
            HExpr::Join { sep, xs } => {
                self.check(sep, tys, st);
                self.check(xs, tys, st);
            }
            HExpr::Subvec { v, start, end, .. } => {
                self.check(v, tys, st);
                self.check(start, tys, st);
                self.check(end, tys, st);
            }
            HExpr::For { .. } => {} // per-iteration: captures are borrows
            HExpr::Conj { v, x, .. } => {
                self.check(v, tys, st);
                // In-place growth requires exclusive ownership: conj moves.
                if let HExpr::Var(name) = &**v {
                    if tys.get(name).map(|t| is_non_copy(*t)).unwrap_or(false) {
                        self.use_var(name, tys, st);
                        st.insert(name.clone(), VState::Moved);
                    }
                }
                self.check(x, tys, st);
            }
            HExpr::Clone(inner) => self.check(inner, tys, st),
            // Creating a closure captures free locals by value: non-Copy
            // captures move.
            HExpr::Closure { captures, .. } => {
                for (name, ty) in captures {
                    self.use_var(name, tys, st);
                    if is_non_copy(*ty) {
                        st.insert(name.clone(), VState::Moved);
                    }
                }
            }
            HExpr::ClosureCall { f, args, .. } => {
                // Calling a closure reads the binding (params get a local
                // copy of the closure value).
                self.check(f, tys, st);
                for a in args {
                    self.check(a, tys, st);
                }
            }
            HExpr::DropStmt(inner) => {
                self.check(inner, tys, st);
                if let HExpr::Var(v) = &**inner {
                    if tys.get(v).map(|t| is_non_copy(*t)).unwrap_or(false) {
                        st.insert(v.clone(), VState::Moved);
                    }
                }
            }
            HExpr::Bin(_, a, b) | HExpr::Cmp(_, a, b, _) => {
                self.check(a, tys, st);
                self.check(b, tys, st);
            }
            HExpr::And(a, b) | HExpr::Or(a, b) => {
                self.check(a, tys, st);
                self.check(b, tys, st);
            }
            HExpr::Not(v) | HExpr::ConvF64(v) | HExpr::Abs(v, _) => self.check(v, tys, st),
            HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
                self.check(a, tys, st);
                self.check(b, tys, st);
            }
            HExpr::Count(v) => self.check(v, tys, st),
            HExpr::Call(name, args) => {
                for a in args {
                    self.check(a, tys, st);
                }
                let kinds = self.kinds_of(name, args.len());
                // Exclusive borrows within one call must not overlap.
                let mut excl_seen: Vec<String> = Vec::new();
                for (arg, kind) in args.iter().zip(kinds.iter()) {
                    if let HExpr::Var(v) = arg {
                        let non_copy = tys.get(v).map(|t| is_non_copy(*t)).unwrap_or(false);
                        if !non_copy {
                            continue;
                        }
                        if *kind == Borrow::Owned {
                            self.use_var(v, tys, st);
                            st.insert(v.clone(), VState::Moved);
                        } else if *kind == Borrow::Exclusive {
                            if excl_seen.contains(v) {
                                self.errs.push(format!(
                                    "cannot borrow {} exclusively more than once in the same call",
                                    v
                                ));
                            }
                            self.use_var(v, tys, st);
                            excl_seen.push(v.clone());
                        }
                    }
                }
            }
            HExpr::Ctor { args, .. } => {
                for a in args {
                    self.check(a, tys, st);
                }
            }
            HExpr::Match { scrut, arms, .. } => {
                self.check(scrut, tys, st);
                // Every arm starts from the same post-scrutinee state: a
                // move in one arm (e.g. a generic-dispatch call) must not
                // poison the other arms — only one arm runs.
                let post_scrut = st.clone();
                let post_tys = tys.clone();
                for arm in arms {
                    let mut arm_tys = post_tys.clone();
                    let mut arm_st = post_scrut.clone();
                    for b in &arm.binders {
                        arm_tys.insert(b.clone(), Ty::I64);
                        arm_st.insert(b.clone(), VState::Owned);
                    }
                    // arm binder types: recover field types are not carried;
                    // binders are element scalars for ADTs (scalar fields
                    // only), so I64 is a safe stand-in for state tracking.
                    self.check(&arm.body, &mut arm_tys, &mut arm_st);
                    // A value moved in any arm counts as moved afterwards.
                    for (k, v) in arm_st.iter() {
                        if *v == VState::Moved {
                            st.insert(k.clone(), VState::Moved);
                        }
                    }
                }
            }
            HExpr::Get { src, idx, default, .. } => {
                self.check(src, tys, st);
                self.check(idx, tys, st);
                if let Some(d) = default {
                    self.check(d, tys, st);
                }
            }
            HExpr::If(c, t, e2, _) => {
                self.check(c, tys, st);
                let mut t_tys = tys.clone();
                let mut t_st = st.clone();
                self.check(t, &mut t_tys, &mut t_st);
                let mut e_tys = tys.clone();
                let mut e_st = st.clone();
                self.check(e2, &mut e_tys, &mut e_st);
                for (k, v) in t_st.iter().chain(e_st.iter()) {
                    if *v == VState::Moved {
                        st.insert(k.clone(), VState::Moved);
                    }
                }
            }
            HExpr::Let(binds, body, _frees) => {
                for (name, value, ty) in binds {
                    self.check(value, tys, st);
                    // Binding a non-Copy value moves it.
                    if let HExpr::Var(v) = value {
                        if is_non_copy(*ty) {
                            st.insert(v.clone(), VState::Moved);
                        }
                    }
                    tys.insert(name.clone(), *ty);
                    st.insert(name.clone(), VState::Owned);
                }
                self.check(body, tys, st);
            }
            HExpr::Seq(vs) => {
                for v in vs {
                    self.check(v, tys, st);
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
// Phase 13 + S1c: scope-exit frees
// ---------------------------------------------------------------------------

/// Heap-owning types: bindings of these types hold a malloc'd buffer that
/// can be released at binding scope exit. Strings and owning ADTs joined
/// the model in S1c; their frees additionally require a fresh (allocating)
/// value shape — see `fresh_owning_value`.
fn heap_owning(ty: Ty, facts: &AdtFacts) -> bool {
    match ty {
        Ty::Vec(_) | Ty::Map | Ty::Set => true,
        Ty::Str => true,
        Ty::Adt(i) => facts.owns.get(i as usize).copied().unwrap_or(false),
        _ => false,
    }
}

/// Does this value expression allocate a fresh heap buffer that the binding
/// uniquely owns? Aliases (`Var`), element loads (`Get`), literals, and
/// control-flow results are deliberately excluded — anything uncertain
/// leaks, nothing double-frees.
fn fresh_owning_value(value: &HExpr, facts: &AdtFacts) -> bool {
    match value {
        // Fresh heap strings.
        HExpr::Subs { .. }
        | HExpr::Split { .. }
        | HExpr::Join { .. }
        | HExpr::StrCat(_)
        | HExpr::CharStr(_) => true,
        // Fresh collections.
        HExpr::Conj { .. }
        | HExpr::Subvec { .. }
        | HExpr::For { .. }
        | HExpr::MapLit(_)
        | HExpr::MapAssoc { .. }
        | HExpr::SetLit(_)
        | HExpr::SetConj { .. }
        | HExpr::SetDisj { .. } => true,
        // A fresh box for boxed/owning ADTs; a fresh collection from a call.
        HExpr::Ctor { adt, .. } => facts.boxed.get(*adt as usize).copied().unwrap_or(false),
        HExpr::Call(_, _) => true,
        _ => false,
    }
}

fn annotate_walk(
    e: &mut HExpr,
    consumed: &HashSet<String>,
    escaped: &HashSet<String>,
    shadowed: &HashSet<String>,
    facts: &AdtFacts,
) {
    match e {
        HExpr::Let(binds, body, frees) => {
            annotate_walk(body, consumed, escaped, shadowed, facts);
            for (name, value, ty) in binds.iter() {
                if heap_owning(*ty, facts)
                    && fresh_owning_value(value, facts)
                    && !consumed.contains(name)
                    && !escaped.contains(name)
                    && !shadowed.contains(name)
                {
                    frees.push(name.clone());
                }
            }
        }
        HExpr::Seq(vs) => {
            for v in vs {
                annotate_walk(v, consumed, escaped, shadowed, facts);
            }
        }
        HExpr::If(c, t, f, _) => {
            annotate_walk(c, consumed, escaped, shadowed, facts);
            annotate_walk(t, consumed, escaped, shadowed, facts);
            annotate_walk(f, consumed, escaped, shadowed, facts);
        }
        HExpr::Loop { binds, body, .. } => {
            for (_, _, e2) in binds {
                annotate_walk(e2, consumed, escaped, shadowed, facts);
            }
            annotate_walk(body, consumed, escaped, shadowed, facts);
        }
        HExpr::Recur(args) => {
            for a in args {
                annotate_walk(a, consumed, escaped, shadowed, facts);
            }
        }
        HExpr::ToStr(e, _) => annotate_walk(e, consumed, escaped, shadowed, facts),
        HExpr::ParseNum { s, .. } => annotate_walk(s, consumed, escaped, shadowed, facts),
        HExpr::Slurp(p) => annotate_walk(p, consumed, escaped, shadowed, facts),
        HExpr::Spit { path, data } => {
            annotate_walk(path, consumed, escaped, shadowed, facts);
            annotate_walk(data, consumed, escaped, shadowed, facts);
        }
        HExpr::Exit(c) => annotate_walk(c, consumed, escaped, shadowed, facts),
        _ => {}
    }
}

/// Annotate every `Let` in a defn body with the bindings that may be freed
/// at scope exit. A binding qualifies when:
/// - its type owns a heap buffer (Vec / Map / Set),
/// - it is never passed as a call argument or used as a `conj` source
///   (conj mutates in place, so consumers alias the buffer),
/// - it is not the function's returned value,
/// - it is not shadowed by another binding of the same name (which would
///   alias the same env slot at codegen).
/// Conservative by design: anything uncertain leaks, nothing double-frees.
pub(crate) fn annotate_frees(body: &mut HExpr, facts: &AdtFacts) {
    let mut consumed: HashSet<String> = HashSet::new();
    let mut escaped: HashSet<String> = HashSet::new();

    fn collect(
        e: &HExpr,
        consumed: &mut HashSet<String>,
        shadowed: &mut HashMap<String, usize>,
    ) {
        match e {
            HExpr::Call(_, args) => {
                for a in args {
                    if let HExpr::Var(v) = a {
                        consumed.insert(v.clone());
                    }
                    collect(a, consumed, shadowed);
                }
            }
            HExpr::Conj { v, x, .. } => {
                if let HExpr::Var(name) = &**v {
                    consumed.insert(name.clone());
                }
                collect(x, consumed, shadowed);
            }
            HExpr::Let(binds, body, _) => {
                for (n, value, _) in binds {
                    shadowed.entry(n.clone()).and_modify(|c| *c += 1).or_insert(1);
                    collect(value, consumed, shadowed);
                }
                collect(body, consumed, shadowed);
            }
            HExpr::Seq(vs) | HExpr::StrCat(vs) => {
                for v in vs {
                    collect(v, consumed, shadowed);
                }
            }
            HExpr::If(c, t, f, _) => {
                collect(c, consumed, shadowed);
                collect(t, consumed, shadowed);
                collect(f, consumed, shadowed);
            }
            HExpr::Loop { binds, body, .. } => {
                for (_, _, e) in binds {
                    collect(e, consumed, shadowed);
                }
                collect(body, consumed, shadowed);
            }
            HExpr::Recur(args) => {
                for a in args {
                    collect(a, consumed, shadowed);
                }
            }
            HExpr::ToStr(e, _) => collect(e, consumed, shadowed),
            HExpr::ParseNum { s, .. } => collect(s, consumed, shadowed),
            HExpr::Slurp(p) => collect(p, consumed, shadowed),
            HExpr::Spit { path, data } => {
                collect(path, consumed, shadowed);
                collect(data, consumed, shadowed);
            }
            HExpr::Exit(c) => collect(c, consumed, shadowed),
            HExpr::And(c, t) | HExpr::Or(c, t) => {
                collect(c, consumed, shadowed);
                collect(t, consumed, shadowed);
            }
            _ => {}
        }
    }


    let mut counts: HashMap<String, usize> = HashMap::new();
    collect(body, &mut consumed, &mut counts);
    tail_escape(body, &mut escaped);
    let shadowed: HashSet<String> = counts
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(n, _)| n)
        .collect();
    annotate_walk(body, &consumed, &escaped, &shadowed, facts);
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
        HExpr::Var(v) => {
            escaped.insert(v.clone());
        }
        _ => {}
    }
}

/// Free a function's owned move-type parameters at body exit: owned
/// vectors/maps/sets and boxed ADTs whose parameter is not the returned
/// value. String (Copy) and inline scalar-ADT params are never freed —
/// value copies share their bytes. The wrapper is an empty-binds `Let`
/// whose free list the backends already emit at scope exit.
pub fn annotate_param_frees(program: &mut Program, facts: &AdtFacts) {
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        let mut escaped: HashSet<String> = HashSet::new();
        tail_escape(&d.body, &mut escaped);
        let frees: Vec<String> = d
            .params
            .iter()
            .enumerate()
            .filter(|(k, (n, ty))| {
                let moves = match ty {
                    Ty::Vec(_) | Ty::Map | Ty::Set => true,
                    Ty::Adt(i) => facts.boxed.get(*i as usize).copied().unwrap_or(false),
                    _ => false,
                };
                moves
                    && !escaped.contains(n)
                    && d.borrows.get(*k).copied().unwrap_or(Borrow::Owned).is_owned()
            })
            .map(|(_, (n, _))| n.clone())
            .collect();
        if !frees.is_empty() {
            let body = std::mem::replace(&mut d.body, HExpr::ConstI64(0));
            d.body = HExpr::Let(Vec::new(), Box::new(body), frees);
        }
    }
}
