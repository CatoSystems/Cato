use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Debug toggles (bisection aids for planner changes): CATO_NO_HOIST,
/// CATO_NO_ARMFREE, CATO_NO_BRANCHFREE, CATO_NO_ESCTRANSFER, CATO_NO_LOOPOWN
/// disable one rule each; CATO_PLAN_LOG/ARMFREE trace decisions.
fn flag(name: &str) -> bool {
    static CACHE: OnceLock<std::collections::HashMap<String, bool>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            ["CATO_NO_HOIST", "CATO_NO_ARMFREE", "CATO_NO_BRANCHFREE", "CATO_NO_ESCTRANSFER", "CATO_NO_LOOPOWN"]
                .iter()
                .map(|n| (n.to_string(), std::env::var_os(n).is_some()))
                .collect()
        })
        .get(name)
        .copied()
        .unwrap_or(false)
}

pub(crate) fn is_non_copy(ty: Ty) -> bool {
    // Closures are move-only owning values (M3): the header uniquely names
    // the env, whose drop glue is invoked at the owner's scope exit.
    matches!(ty, Ty::Adt(_) | Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Fn(_))
}

/// Does a match binder over this field type alias the scrutinee's box?
/// Str fields ride as strdup'd pointers the box's drop glue frees; vec/map/
/// set fields live in the box's payload; boxed ADT children are pointers
/// into the same allocation graph. Scalars and inline-ADT children copy by
/// value, so their binders may escape or move freely.
pub(crate) fn binder_field_aliases(fty: Option<Ty>, boxed: &[bool]) -> bool {
    match fty {
        Some(Ty::Str) | Some(Ty::Vec(_)) | Some(Ty::Map) | Some(Ty::Set) | Some(Ty::Fn(_)) => true,
        Some(Ty::Adt(j)) => boxed.get(j as usize).copied().unwrap_or(false),
        _ => false,
    }
}

/// The ADT index of a match scrutinee, for the boxed-alias analysis: a
/// named var from the binding map, anything else from its static type
/// (the callee's return type, a nested match/if result, an Option `res`)
/// — mirroring the backend's `adt_of`. `None` = not an ADT (or unknown),
/// which leaves the binders at the I64 copy stand-in as before.
fn scrutinee_adt(
    e: &HExpr,
    tys: &HashMap<String, Ty>,
    rets: &HashMap<String, Ty>,
) -> Option<u32> {
    match e {
        HExpr::Var(v) => match tys.get(v) {
            Some(Ty::Adt(i)) => Some(*i),
            _ => None,
        },
        HExpr::Ctor { adt, .. } => Some(*adt),
        HExpr::Match { ty: Ty::Adt(i), .. } => Some(*i),
        HExpr::If(_, _, _, Ty::Adt(i)) => Some(*i),
        HExpr::Clone(inner) => scrutinee_adt(inner, tys, rets),
        // The planner's own `let` around a scrutinee call (hoisted argument
        // temps, freed before the arms run) has the call's type.
        HExpr::Let(_, body, _) => scrutinee_adt(body, tys, rets),
        // Let/Seq scrutinees are rejected in `check` (their owning box frees
        // inside a scope the binder-escape suppression cannot reach), so
        // they never reach the alias analysis here. A loop's recorded
        // completing type is safe: the temp is never freed, so binders
        // alias a leaked value and moves through them stay sound.
        HExpr::Loop { ty: Ty::Adt(i), .. } => Some(*i),
        HExpr::Call(name, _) => match rets.get(name) {
            Some(Ty::Adt(i)) => Some(*i),
            _ => None,
        },
        HExpr::StrNth { res: Some(Ty::Adt(i)), .. }
        | HExpr::Get { res: Some(Ty::Adt(i)), .. }
        | HExpr::MapGet { res: Some(Ty::Adt(i)), .. } => Some(*i),
        HExpr::ParseNum { res: Ty::Adt(i), .. } => Some(*i),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum VState {
    Owned,
    Moved,
}

/// One boxed-scrutinee match arm under checking: its scrutinee var and the
/// binders that alias the scrutinee's heap box. The backend materializes
/// str/vec/ADT binders as pointers into (or owned by) that box, so when the
/// arm moves the scrutinee itself — handing the box to a callee that frees
/// it — every later use of an aliasing binder reads freed memory. Moving
/// the scrutinee retires the binders (marks them `Moved` in the state map,
/// where branch isolation and the use-of-moved error come for free).
struct Watch {
    scrutinee: String,
    binders: HashSet<String>,
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
    /// Callees whose return value is a borrow (`&T`): a call to one of
    /// these binds a non-owning value, and passing that value to an owned
    /// parameter is a move of a borrow (E303).
    pub(crate) ret_borrowed_names: &'a HashSet<String>,
    /// Is this defn itself a borrowed-returning (`&T`) function? Its tail
    /// is a borrow, so a match binder over a `&` scrutinee legitimately
    /// escapes (no "clone it first" error).
    pub(crate) ret_borrowed: bool,
    /// Parameters the function does not own (`&`): they still belong
    /// to the caller, so moving them anywhere in the body is an error.
    pub(crate) no_move: HashSet<String>,
    /// Representation facts (boxing/ownership) for ADT eligibility.
    pub(crate) facts: &'a AdtFacts,
    /// The ADT table, for binder field types (alias detection below).
    pub(crate) adts: &'a [AdtDef],
    /// Boxedness over the full (template + instance) table: match
    /// scrutinees carry combined-table indices, which the build-stage
    /// facts (templates only) cannot answer.
    pub(crate) boxed_full: Vec<bool>,
    /// Names in tail (returned) position: the caller inherits their drops.
    pub(crate) escaped: HashSet<String>,
    /// Non-Copy loop variables (S8): the checker treats them as Copy —
    /// reads never move, calls never consume, and no scope frees one.
    pub(crate) loop_copy: HashSet<String>,
    /// Consumed names (see struct docs).
    pub(crate) consumed: HashSet<String>,
    /// Remaining occurrences of each name (pre-counted); a conj source whose
    /// count hits 0 at the conj is a last use, so in-place update is
    /// unobservable.
    pub(crate) remaining: HashMap<String, i64>,
    /// Callee name -> return type, for boxed-scrutinee detection when the
    /// scrutinee is a fresh temp (a call, a nested match, a clone — anything
    /// but a named var, whose type lives in the binding map).
    pub(crate) rets: &'a HashMap<String, Ty>,
    /// Callees whose string/collection result may alias a parameter
    /// (`compute_alias_ret`): such results are never fresh.
    pub(crate) alias_ret: &'a HashSet<String>,
    /// `ret_borrowed_names` ∪ `alias_ret`: the callees through which a
    /// value can alias an argument.
    alias_calls: HashSet<String>,
    /// Params whose buffers free at body exit (result, when requested).
    pub(crate) param_frees: Vec<String>,
    /// Boxed-scrutinee arms on the check path (innermost last); see `Watch`.
    watches: Vec<Watch>,
    /// While a `recur` argument is being checked: the accumulator whose
    /// conj is the loop's own self-update (the planner mirror of the
    /// backend's `recur_fast`). The backend proves the append unobservable
    /// for this shape; pairing it with `loop_seed_fresh` proves the old
    /// buffer uniquely owned, so the conj is a last use even though the
    /// completing tail also names the accumulator.
    recur_ok: Option<String>,
    /// Loop variables whose seed allocated a fresh buffer (not a moved
    /// Var: a transferred buffer is also reachable through the seed's old
    /// binding slot, and the backend keeps those copying — `loop_alias`).
    loop_seed_fresh: HashSet<String>,
    pub(crate) errs: Vec<String>,
    /// The definition under check (diagnostics only).
    dname: String,
    /// Aliasing binders of the innermost enclosing match arms whose box is
    /// freed at arm exit (name -> is the box freeable). A move of such a
    /// binder on one branch leaves its field for the glue to skip; the
    /// other branches must then free it themselves (`branch_frees`).
    freeable_alias: HashMap<String, bool>,
    /// Names minted for hoisted call-argument temps (`~a0`, `~a1`, …).
    tmp_counter: usize,
    /// Error sites (see `diagnostics::refine_occurrence`): per symbol, how
    /// many of its source occurrences the walk has reached — a variable's
    /// value-position reads, a closure capture (one per `fn` form), a
    /// call's head, a `match` head.
    occ: HashMap<String, usize>,
    /// The occurrence being processed (a move re-reads its argument after
    /// the whole call was checked, so the walk sets this explicitly).
    cur: Option<(String, usize)>,
    /// Where each binding was last moved (for errors raised later).
    moved_at: HashMap<String, (String, usize)>,
    /// The first error's site, if it has one.
    site: Option<(String, usize)>,
}

/// Check move semantics and plan drops for one body. With
/// `with_param_frees`, owned move-type parameters that still hold their
/// buffer at body exit get a scope-exit free (an empty-binds `Let` wrapper,
/// which the backends already emit).
pub(crate) fn plan_drops(
    name: &str,
    params: &[(String, Ty)],
    borrows: &[Borrow],
    ret: Ty,
    ret_borrowed: bool,
    body: &mut HExpr,
    sigs: &HashMap<String, Vec<Borrow>>,
    ret_borrowed_names: &HashSet<String>,
    rets: &HashMap<String, Ty>,
    facts: &AdtFacts,
    adts: &[AdtDef],
    with_param_frees: bool,
    alias_ret: &HashSet<String>,
) -> Result<(), String> {
    let mut escaped = HashSet::new();
    tail_escape(body, &mut escaped);

    let mut var_counts: HashMap<String, i64> = HashMap::new();
    count_vars(body, &mut var_counts);
    let boxed_full = compute_adt_facts(adts).boxed;

    // Borrowed-return preconditions. Both fire before the body check so the
    // reported error is the return-shape problem, not a downstream symptom
    // (e.g. a borrowed param moved into a fresh constructor).
    if ret_borrowed {
        // `&T` is only a sound borrow for pointer-like values: boxed
        // (recursive/owning) ADTs. Scalars and inline ADTs copy by value,
        // so a borrow of them is meaningless (and a `&` on the return
        // annotation cannot mark an Inline slot).
        let boxed_ret = matches!(ret, Ty::Adt(i) if boxed_full.get(i as usize).copied().unwrap_or(false));
        if !boxed_ret {
            return Err(
                "borrowed return: `&T` requires a boxed (recursive or owning) ADT return type"
                    .to_string(),
            );
        }
        // No lifetimes: the returned borrow must be derived from the `&`
        // parameters, transitively. An owned tail (a fresh constructor, an
        // owned local, a clone) would dangle once the caller frees it.
        let borrowed_params: HashSet<String> = params
            .iter()
            .zip(borrows.iter())
            .filter(|(_, b)| !b.is_owned())
            .map(|((n, _), _)| n.clone())
            .collect();
        if !tail_borrow_ok(body, &borrowed_params, sigs, ret_borrowed_names) {
            return Err(
                "Borrowed return requires a borrowed value. A `&T` function may only return a value derived from its `&` parameters, never an owned or freshly-cloned value."
                    .to_string(),
            );
        }
    } else if !ret.is_copy_ty() {
        // The inverse discipline (B8): a function whose return type *moves*
        // may not return a value derived from a borrowed parameter. The
        // caller binds the result as owned and frees it at scope exit while
        // the parameter's owner frees the same buffer — the confirmed
        // double-free behind `(defn f [b :- &Box] :- Box b)`. Marked `&T`
        // returns are exempt above; Copy returns carry no duty.
        let borrowed_params: HashSet<String> = params
            .iter()
            .zip(borrows.iter())
            .filter(|(_, b)| !b.is_owned())
            .map(|((n, _), _)| n.clone())
            .collect();
        if !borrowed_params.is_empty()
            && tail_borrow_ok(body, &borrowed_params, sigs, ret_borrowed_names)
        {
            return Err(
                "a function returning a move type may not return a value derived from a `&` parameter: the caller would free a buffer the parameter's owner still frees. Mark the return `&T`, or clone the value before returning it."
                    .to_string(),
            );
        }
    }

    let mut bc = BCheck {
        sigs,
        ret_borrowed_names,
        ret_borrowed,
        no_move: params
            .iter()
            .zip(borrows.iter())
            .filter(|(_, b)| !b.is_owned())
            .map(|((n, _), _)| n.clone())
            .collect(),
        facts,
        adts,
        boxed_full,
        escaped,
        loop_copy: HashSet::new(),
        consumed: HashSet::new(),
        remaining: var_counts,
        rets,
        alias_ret,
        alias_calls: ret_borrowed_names.union(alias_ret).cloned().collect(),
        param_frees: Vec::new(),
        watches: Vec::new(),
        recur_ok: None,
        loop_seed_fresh: HashSet::new(),
        errs: Vec::new(),
        dname: name.to_string(),
        freeable_alias: HashMap::new(),
        tmp_counter: 0,
        occ: HashMap::new(),
        cur: None,
        moved_at: HashMap::new(),
        site: None,
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
        if let Some((sym, k)) = bc.site.take() {
            crate::diagnostics::set_occurrence(&sym, k);
        }
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

/// Is `name` re-bound by a `let` anywhere inside this expression? The
/// scope-exit free stays suppressed only then — that is the one shape where
/// the old "shadowed names share a slot" concern is real. Sibling-branch
/// lets of the same name are distinct bindings in distinct registers (the
/// backend save/restores per scope; the interpreter uses child envs), so
/// each frees its own buffer.
fn rebound_in_body(e: &HExpr, name: &str) -> bool {
    match e {
        HExpr::Let(binds, body, _) => {
            binds.iter()
                .any(|(n, v, _)| n == name || rebound_in_body(v, name))
                || rebound_in_body(body, name)
        }
        HExpr::Seq(vs) => vs.iter().any(|v| rebound_in_body(v, name)),
        HExpr::If(c, t, f, _) => {
            rebound_in_body(c, name)
                || rebound_in_body(t, name)
                || rebound_in_body(f, name)
        }
        HExpr::Match { scrut, arms, .. } => {
            rebound_in_body(scrut, name)
                || arms.iter().any(|a| rebound_in_body(&a.body, name))
        }
        HExpr::Loop { binds, body, .. } => {
            binds.iter().any(|(_, _, i)| rebound_in_body(i, name))
                || rebound_in_body(body, name)
        }
        HExpr::Recur(args) => args.iter().any(|a| rebound_in_body(a, name)),
        HExpr::And(a, b) | HExpr::Or(a, b) => {
            rebound_in_body(a, name) || rebound_in_body(b, name)
        }
        _ => false,
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

/// Static, pre-check test for a borrowed-return tail (no lifetimes): can
/// evaluating `e` yield a borrow derived from the `&` parameters? `borrows`
/// starts as the borrowed parameter names; match binders over a borrowed
/// scrutinee and `let` bindings of a borrow are added as we descend. A call
/// to another `&T` function yields a borrow only when every value handed to
/// that callee's own `&` parameters is itself a borrow — otherwise the
/// returned borrow would derive from a temporary. Anything that mints a
/// fresh value (constructor, clone, literal, collection) is owned and
/// rejects. The test is conservative: an unmodelled form returns `false`
/// (reject), never a false accept.
fn tail_borrow_ok(
    e: &HExpr,
    borrows: &HashSet<String>,
    sigs: &HashMap<String, Vec<Borrow>>,
    ret_borrowed_names: &HashSet<String>,
) -> bool {
    match e {
        HExpr::Var(v) => borrows.contains(v),
        HExpr::Call(name, args) => {
            if !ret_borrowed_names.contains(name) {
                return false;
            }
            let kinds = sigs.get(name).cloned().unwrap_or_default();
            args.iter().zip(kinds.iter()).all(|(a, k)| {
                k.is_owned() || tail_borrow_ok(a, borrows, sigs, ret_borrowed_names)
            })
        }
        HExpr::Clone(_) => false,
        HExpr::If(_, then, els, _) => {
            tail_borrow_ok(then, borrows, sigs, ret_borrowed_names)
                && tail_borrow_ok(els, borrows, sigs, ret_borrowed_names)
        }
        HExpr::Match { scrut, arms, .. } => {
            // Binders alias a borrow only when the scrutinee is itself a
            // borrow (`&` param or a binder introduced above); a `match` on
            // an owned scrutinee hands back owned (or copy) values.
            let scrut_borrow = matches!(&**scrut, HExpr::Var(v) if borrows.contains(v))
                || tail_borrow_ok(scrut, borrows, sigs, ret_borrowed_names);
            arms.iter().all(|arm| {
                let mut arm_borrows = borrows.clone();
                if scrut_borrow {
                    arm_borrows.extend(arm.binders.iter().cloned());
                }
                tail_borrow_ok(&arm.body, &arm_borrows, sigs, ret_borrowed_names)
            })
        }
        HExpr::Let(binds, body, _) => {
            let mut inner = borrows.clone();
            for (n, value, _) in binds {
                if tail_borrow_ok(value, &inner, sigs, ret_borrowed_names) {
                    inner.insert(n.clone());
                }
            }
            tail_borrow_ok(body, &inner, sigs, ret_borrowed_names)
        }
        HExpr::Seq(vs) => vs
            .last()
            .map(|last| tail_borrow_ok(last, borrows, sigs, ret_borrowed_names))
            .unwrap_or(false),
        _ => false,
    }
}

/// May the value of `e` be a string/collection buffer the caller must not
/// free — a result of a function in `nonfresh` (an extern hands back
/// runtime-owned memory; a wrapper hands it on), a `let` alias of one, a
/// loop variable seeded with one or with a literal (static data)?
fn tail_nonfresh(e: &HExpr, nonfresh: &HashSet<String>, vars: &HashSet<String>) -> bool {
    match e {
        HExpr::Call(c, _) => nonfresh.contains(c),
        HExpr::Var(v) => vars.contains(v),
        // A string-returning function's literal tails are materialized as
        // owned copies by the backend (`own_str_return`), so a literal is
        // fresh there. A vector literal has no such copy: it is static.
        HExpr::VecLit(_) => true,
        HExpr::Let(binds, body, _) => {
            let mut inner = vars.clone();
            for (n, value, _) in binds {
                if tail_nonfresh(value, nonfresh, &inner) {
                    inner.insert(n.clone());
                }
            }
            tail_nonfresh(body, nonfresh, &inner)
        }
        HExpr::Seq(vs) => vs
            .last()
            .map(|l| tail_nonfresh(l, nonfresh, vars))
            .unwrap_or(false),
        HExpr::If(_, t, f, _) => tail_nonfresh(t, nonfresh, vars) || tail_nonfresh(f, nonfresh, vars),
        HExpr::Match { arms, .. } => arms.iter().any(|a| tail_nonfresh(&a.body, nonfresh, vars)),
        HExpr::Loop { binds, body, .. } => {
            let mut inner = vars.clone();
            for (n, _, init) in binds {
                if tail_nonfresh(init, nonfresh, &inner) {
                    inner.insert(n.clone());
                }
            }
            let mut leaves = Vec::new();
            tail_leaves(body, &mut leaves);
            leaves
                .iter()
                .any(|l| !matches!(l, HExpr::Recur(_)) && tail_nonfresh(l, nonfresh, &inner))
        }
        _ => false,
    }
}

/// Functions whose string or collection result is not the caller's to free:
/// it may be one of their own parameters' buffers (`(defn id [s :- str] :-
/// str s)`), runtime-owned memory from an extern, or static data — directly
/// or through another such function. Computed to a fixpoint over the
/// program; callers neither own nor free these results.
pub fn compute_alias_ret(defns: &[Defn]) -> HashSet<String> {
    let coll = |t: Ty| matches!(t, Ty::Str | Ty::Vec(_) | Ty::Map | Ty::Set);
    let mut calls: HashSet<String> = defns
        .iter()
        .filter(|d| d.ret_borrowed)
        .map(|d| d.name.clone())
        .collect();
    let mut out: HashSet<String> = defns
        .iter()
        .filter(|d| d.is_extern && coll(d.ret))
        .map(|d| d.name.clone())
        .collect();
    loop {
        let mut changed = false;
        for d in defns {
            if d.is_extern || out.contains(&d.name) || !coll(d.ret) {
                continue;
            }
            let params: HashSet<String> = d
                .params
                .iter()
                .filter(|(_, t)| coll(*t))
                .map(|(n, _)| n.clone())
                .collect();
            if tail_may_alias(&d.body, &params, &calls)
                || tail_nonfresh(&d.body, &out, &HashSet::new())
            {
                out.insert(d.name.clone());
                calls.insert(d.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// Is this match scrutinee a fresh heap box nobody else holds — a call
/// result (an owned return; a `&T` call hands back its origin's box), a
/// boxed constructor, a deep clone? Merges (`if`/`match`) may yield an
/// alias and Option materializations (`get`/`nth`/`parse-*`) live on the
/// stack, so they do not count.
fn fresh_box(e: &HExpr, ret_borrowed_names: &HashSet<String>) -> bool {
    match e {
        HExpr::Call(name, _) => !ret_borrowed_names.contains(name),
        HExpr::Ctor { .. } | HExpr::Clone(_) => true,
        // A planner-made `let` around a call (hoisted argument temps).
        HExpr::Let(_, body, _) => fresh_box(body, ret_borrowed_names),
        _ => false,
    }
}

/// Does the tail of `e` jump back to a loop head? Such an arm has no exit
/// of its own to free at.
fn tail_has_recur(e: &HExpr) -> bool {
    match e {
        HExpr::Recur(_) => true,
        HExpr::Let(_, body, _) => tail_has_recur(body),
        HExpr::Seq(vs) => vs.last().map(tail_has_recur).unwrap_or(false),
        HExpr::If(_, t, f, _) => tail_has_recur(t) || tail_has_recur(f),
        HExpr::Match { arms, .. } => arms.iter().any(|a| tail_has_recur(&a.body)),
        _ => false,
    }
}

/// The expressions that can be the value of `e`: one per path through its
/// tail-position `let`/`do`/`if`/`match` structure.
fn tail_leaves<'e>(e: &'e HExpr, out: &mut Vec<&'e HExpr>) {
    match e {
        HExpr::Let(_, body, _) => tail_leaves(body, out),
        HExpr::Seq(vs) => {
            if let Some(last) = vs.last() {
                tail_leaves(last, out);
            }
        }
        HExpr::If(_, t, f, _) => {
            tail_leaves(t, out);
            tail_leaves(f, out);
        }
        HExpr::Match { arms, .. } => {
            for a in arms {
                tail_leaves(&a.body, out);
            }
        }
        other => out.push(other),
    }
}

/// May the value of `e` point into memory reachable from the names in
/// `alias` (a match's aliasing binders, a function's parameters)? The
/// any-path dual of `tail_borrow_ok`: a bare alias, a call over one to a
/// function in `alias_calls` (a `&T` callee, or one whose string/collection
/// result may be a parameter — see `compute_alias_ret`), a `let` alias of
/// one, an element load from an aliased collection, a loop seeded from
/// one. Fresh values (constructors, clones, owned calls, literals) do not.
pub fn tail_may_alias(
    e: &HExpr,
    alias: &HashSet<String>,
    alias_calls: &HashSet<String>,
) -> bool {
    let ret_borrowed_names = alias_calls;
    match e {
        HExpr::Var(v) => alias.contains(v),
        HExpr::Call(name, args) => {
            ret_borrowed_names.contains(name)
                && args.iter().any(|a| tail_may_alias(a, alias, ret_borrowed_names))
        }
        HExpr::Let(binds, body, _) => {
            let mut inner = alias.clone();
            for (n, value, _) in binds {
                if tail_may_alias(value, &inner, ret_borrowed_names) {
                    inner.insert(n.clone());
                }
            }
            tail_may_alias(body, &inner, ret_borrowed_names)
        }
        HExpr::Seq(vs) => vs
            .last()
            .map(|last| tail_may_alias(last, alias, ret_borrowed_names))
            .unwrap_or(false),
        HExpr::If(_, t, f, _) => {
            tail_may_alias(t, alias, ret_borrowed_names)
                || tail_may_alias(f, alias, ret_borrowed_names)
        }
        HExpr::Match { scrut, arms, .. } => {
            let scrut_alias = tail_may_alias(scrut, alias, ret_borrowed_names);
            arms.iter().any(|arm| {
                let mut inner = alias.clone();
                if scrut_alias {
                    inner.extend(arm.binders.iter().cloned());
                }
                tail_may_alias(&arm.body, &inner, ret_borrowed_names)
            })
        }
        // A string element points into its vector's buffer; an Option
        // result carries that same pointer; an out-of-range read hands the
        // default back as is.
        HExpr::Get { src, default, ty, res, .. } => {
            ((*ty == Ty::Str || res.is_some()) && tail_may_alias(src, alias, ret_borrowed_names))
                || default
                    .as_ref()
                    .is_some_and(|d| tail_may_alias(d, alias, ret_borrowed_names))
        }
        HExpr::MapGet { src, default, .. } => {
            tail_may_alias(src, alias, ret_borrowed_names)
                || default
                    .as_ref()
                    .is_some_and(|d| tail_may_alias(d, alias, ret_borrowed_names))
        }
        HExpr::Loop { binds, body, .. } => {
            binds
                .iter()
                .any(|(_, _, init)| tail_may_alias(init, alias, ret_borrowed_names))
                || tail_may_alias(body, alias, ret_borrowed_names)
        }
        _ => false,
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

    /// The next source occurrence of `sym` is being processed.
    fn bump(&mut self, sym: &str) -> (String, usize) {
        let k = self.occ.entry(sym.to_string()).or_insert(0);
        *k += 1;
        let site = (sym.to_string(), *k);
        self.cur = Some(site.clone());
        site
    }

    /// The site of the occurrence of `v` being processed: the current one
    /// when it is `v`'s, else `v`'s latest.
    fn site_for(&self, v: &str) -> (String, usize) {
        match &self.cur {
            Some((n, k)) if n == v => (n.clone(), *k),
            _ => (v.to_string(), self.occ.get(v).copied().unwrap_or(0)),
        }
    }

    /// Record an error; the first one's site locates the diagnostic.
    fn err(&mut self, msg: String, site: Option<(String, usize)>) {
        if self.errs.is_empty() {
            self.site = site;
        }
        self.errs.push(msg);
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
            if is_non_copy(*ty)
                && !self.loop_copy.contains(v)
                && st.get(v) == Some(&VState::Moved)
            {
                let site = self.site_for(v);
                self.err(format!("use of moved value {}", v), Some(site));
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
            let site = self.site_for(v);
            self.err(
                format!(
                    "cannot move {}: it is a borrowed parameter and still belongs to the caller",
                    v
                ),
                Some(site),
            );
            return;
        }
        let at = self.site_for(v);
        self.moved_at.insert(v.to_string(), at);
        // Moving a watched scrutinee retires its arm's aliasing binders:
        // their values live in (or own through) the box that just left.
        // The state map carries the retirement — a later use is the
        // standard use-of-moved error, and per-branch maps keep the mark
        // out of sibling branches.
        let retired: Vec<String> = self
            .watches
            .iter()
            .filter(|w| w.scrutinee == v)
            .flat_map(|w| w.binders.iter().cloned())
            .collect();
        for b in retired {
            st.insert(b.clone(), VState::Moved);
            owns.insert(b, false);
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
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        // Non-Copy loop variables ride as Copy: a call argument that is a
        // loop variable neither moves nor consumes (S8).
        if self.loop_copy.contains(v) {
            self.use_var(v, tys, st);
            return;
        }
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
        // A fresh boxed value handed to a borrowed parameter has no owner:
        // the callee only reads it and the caller never named it. Bind it
        // first, so the binding's scope free releases it after the call.
        if !flag("CATO_NO_HOIST") {
            self.hoist_arg_temps(e, tys);
        }
        match e {
            HExpr::Var(v) => {
                let v = v.clone();
                self.bump(&v);
                self.use_var(&v, tys, st);
            }
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
                let mut shadowed_owns: Vec<(String, Option<bool>)> = Vec::new();
                for (n, ty, init) in binds.iter_mut() {
                    self.check(init, tys, st, owns);
                    // A non-Copy Var initializer moves into the loop: the
                    // value rides the loop slot for the whole scan and no
                    // scope ever frees it, so the source's drop duty ends
                    // here (without this, a boxed parameter that seeds a
                    // loop would free live state at body exit).
                    let mut init_owns = false;
                    let mut seed_fresh = false;
                    if let HExpr::Var(v) = init {
                        if !ty.is_copy_ty() && !self.loop_copy.contains(v) {
                            // The loop slot inherits the source's duty. A
                            // transferred buffer stays reachable through
                            // the seed's old slot: owned, but never a
                            // fresh seed (the backend keeps those copying).
                            init_owns = owns.get(v).copied().unwrap_or(false);
                            self.move_var(v, st, owns);
                        }
                    } else if !ty.is_copy_ty() && allocates(init, self.facts) {
                        init_owns = true;
                        seed_fresh = true;
                    }
                    if seed_fresh {
                        self.loop_seed_fresh.insert(n.clone());
                    }
                    shadowed_vars.push((n.clone(), st.get(n).copied()));
                    shadowed_owns.push((n.clone(), owns.get(n).copied()));
                    tys.insert(n.clone(), *ty);
                    st.insert(n.clone(), VState::Owned);
                    // The loop slot owns the buffer it rides (a fresh
                    // allocation, or the duty a moved seed transferred).
                    // Without this the planner can never prove a last use
                    // on the accumulator, so `conj` stays copying and the
                    // backend's grow path never frees the old buffer —
                    // every geometric growth leaked.
                    owns.insert(n.clone(), init_owns);
                    // Non-Copy loop variables ride as Copy (S8): reads and
                    // calls never move them, and no scope frees them.
                    if !ty.is_copy_ty() {
                        self.loop_copy.insert(n.clone());
                    }
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
                    self.loop_copy.remove(n);
                    self.loop_seed_fresh.remove(n);
                }
                // owns restores with st: a stale owns=true leaking past the
                // loop would let a later conj on the shadowed outer binding
                // fire in-place on a buffer it only aliases.
                for (n, prev) in shadowed_owns.iter().rev() {
                    match prev {
                        Some(o) => {
                            owns.insert(n.clone(), *o);
                        }
                        None => {
                            owns.remove(n);
                        }
                    }
                }
            }
            HExpr::Recur(args) => {
                // The self-update shape — `(recur ... (conj acc x) ...)` —
                // is the loop's accumulator update: the backend proves the
                // append unobservable there (the slot is rebound right
                // after, and every read of the old header carries the
                // pre-append length), so the conj is a last use of the
                // accumulator even though the loop's completing tail also
                // names it. Conditions mirror the backend's `recur_fast`:
                // single conj of a fresh-seeded loop variable, and no
                // sibling argument reads it (a later arg reading the old
                // header would observe the append — or the growth free).
                let mut conj_counts: HashMap<String, i64> = HashMap::new();
                for a in args.iter() {
                    if let HExpr::Conj { v, .. } = a {
                        if let HExpr::Var(n) = &**v {
                            *conj_counts.entry(n.clone()).or_insert(0) += 1;
                        }
                    }
                }
                for ai in 0..args.len() {
                    // Is args[ai] the loop's self-update (mirroring the
                    // backend's recur_fast conditions)? Computed before the
                    // mutable borrow of the argument below.
                    let candidate = match &args[ai] {
                        HExpr::Conj { v, .. } => match &**v {
                            HExpr::Var(n)
                                if conj_counts.get(n) == Some(&1)
                                    && self.loop_copy.contains(n)
                                    && self.loop_seed_fresh.contains(n) =>
                            {
                                let sibling_reads = args.iter().enumerate().any(|(k, o)| {
                                    k != ai && {
                                        let mut m: HashMap<String, i64> = HashMap::new();
                                        count_vars(o, &mut m);
                                        m.get(n).copied().unwrap_or(0) > 0
                                    }
                                });
                                if sibling_reads {
                                    None
                                } else {
                                    Some(n.clone())
                                }
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    self.recur_ok = candidate;
                    let a = &mut args[ai];
                    self.check(a, tys, st, owns);
                }
                self.recur_ok = None;
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
            HExpr::For {
                el,
                src,
                elem,
                body,
                ..
            } => {
                self.check(src, tys, st, owns);
                // The body runs once per element: it must be checked, and
                // against the *live* state (not a per-branch clone), so a
                // use-after-move inside the body is seen textually. A move
                // of a binding that predates the loop can never be sound —
                // the second iteration would read a moved value — so any
                // such move is an error after the walk. Bindings created
                // inside the body (including `el` and nested match binders)
                // are per-iteration and may move freely: their temporaries
                // are freshly minted each iteration.
                let elem_ty = match elem {
                    Elem::I64 => Ty::I64,
                    Elem::F64 => Ty::F64,
                    Elem::Str => Ty::Str,
                    Elem::Param(k) => Ty::Param(*k),
                };
                let pre: HashSet<String> = st.keys().cloned().collect();
                let pre_moved: HashSet<String> = st
                    .iter()
                    .filter(|(_, s)| **s == VState::Moved)
                    .map(|(k, _)| k.clone())
                    .collect();
                tys.insert(el.clone(), elem_ty);
                st.insert(el.clone(), VState::Owned);
                self.check(body, tys, st, owns);
                self.forbid_pred_moves(&pre, &pre_moved, st, tys, "a `for` body");
            }
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
                        && (self.remaining.get(&name).copied() == Some(0)
                            || self.recur_ok.as_deref() == Some(name.as_str()));
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
                    // A capture is one occurrence: the `fn` form itself.
                    self.bump(name);
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
                self.bump(name);
                let mut arg_sites: Vec<Option<(String, usize)>> = Vec::new();
                for a in args.iter_mut() {
                    self.check(a, tys, st, owns);
                    arg_sites.push(match a {
                        HExpr::Var(v) => Some(self.site_for(v)),
                        _ => None,
                    });
                }
                let kinds = self.kinds_of(name, args.len());
                for ((arg, kind), asite) in args.iter().zip(kinds.iter()).zip(arg_sites) {
                    if asite.is_some() {
                        self.cur = asite;
                    }
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
                    } else if let HExpr::Call(callee, _) = arg {
                        // A borrowed (`&T`) call result handed to an owned
                        // parameter would let the callee free a value that
                        // still belongs to the borrow's origin.
                        if *kind == Borrow::Owned && self.ret_borrowed_names.contains(callee) {
                            self.err(
                                format!(
                                    "cannot move the borrowed result of {} into an owned parameter of {}: it belongs to the caller",
                                    callee, name
                                ),
                                Some(self.site_for(callee)),
                            );
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
                let match_site = self.bump("match");
                self.check(scrut, tys, st, owns);
                // A let-wrapped scrutinee owns its box in a scope the
                // binder-escape suppression below cannot reach (the
                // suppression targets a *named* scrutinee): a moved binder
                // plus the let's own scope free would double-free. Require
                // the binding form instead — `(let [x (f)] (match x ...))`
                // — where every existing rule composes. (Seq scrutinees
                // share the shape; their tail var is owned by an enclosing
                // binding the match cannot suppress either.)
                let user_let = match &**scrut {
                    HExpr::Let(binds, _, _) => binds.iter().any(|(n, _, _)| !n.starts_with('~')),
                    HExpr::Seq(_) => true,
                    _ => false,
                };
                if user_let {
                    self.err(
                        "match scrutinee must not be a `let`/`do` form: bind it to a name first (its buffer would be freed before the arms run)".to_string(),
                        Some(match_site.clone()),
                    );
                }
                // Every arm starts from the same post-scrutinee state: a
                // move in one arm (e.g. a generic-dispatch call) must not
                // poison the other arms — only one arm runs.
                let post_scrut = st.clone();
                let post_owns = owns.clone();
                let post_tys = tys.clone();
                // A boxed scrutinee keeps every field inside one heap box;
                // its binders are aliases into that box. A binder that
                // escapes the arm or is moved would outlive the box once
                // the scrutinee frees at scope exit, so the source must
                // clone it (the same discipline collections follow at
                // every boundary). Scalar and inline-ADT fields copy by
                // value and may flow freely.
                // The scrutinee's own type (the match's `ty` field is the
                // result's). Named scrutinees take the type from the
                // binding map; everything else (a call temp, a nested
                // match, a clone) from the static callee/result types —
                // the alias analysis below must see boxedness for fresh
                // temps too, or moves through their binders go untracked
                // and a twice-consumed binder is a use-after-move the
                // check never sees (S8 — the emit-program UAF).
                let scrut_adt = scrutinee_adt(scrut, tys, self.rets);
                let scrut_boxed = scrut_adt
                    .map(|i| self.boxed_full.get(i as usize).copied().unwrap_or(false))
                    .unwrap_or(false);
                // Boxed-scrutinee binder analysis, refined by S8. The
                // binders alias the scrutinee's one heap box, so the box
                // must outlive every alias:
                // - A binder that escapes the arm or moves out transfers
                //   the box's drop duty to its new holder — the scrutinee's
                //   own scope free is suppressed (the box leaks through the
                //   alias). Without this, threading a match binder into a
                //   recursive call forced a whole-chain clone at every
                //   step, which made self-compilation quadratic.
                // - If no aliasing binder escapes, the scrutinee frees at
                //   its own scope exit exactly as before (drop duty runs
                //   exactly once, in the final consumer).
                // - The one unsound case is a borrowed (`&`) boxed
                //   parameter: the caller owns the box and frees it, so its
                //   binders must stay inside the arm (an error below).
                let scrut_borrowed_param = match &**scrut {
                    HExpr::Var(sv) => self.no_move.contains(sv),
                    _ => false,
                };
                // Which boxes can this match itself free, at each arm's
                // exit? Until now a boxed scrutinee was freed only by its
                // owner's scope: a call/ctor/clone temp has no owner and was
                // never freed, and a named box whose arm transferred an alias
                // out lost its scope free wholesale — every sibling field
                // leaked with the box. Both shapes are the kernel's own
                // self-build footprint (85% of its allocations never freed).
                // The arm-exit free releases the box once its value is
                // computed, with the transferred binders' slots nulled so
                // the glue skips exactly the objects somebody else now holds.
                let sv_name: Option<String> = match &**scrut {
                    HExpr::Var(sv) => Some(sv.clone()),
                    _ => None,
                };
                let temp_fresh = scrut_boxed
                    && sv_name.is_none()
                    && fresh_box(scrut, self.ret_borrowed_names);
                let var_owned = scrut_boxed
                    && sv_name.as_ref().is_some_and(|sv| {
                        owns.get(sv).copied().unwrap_or(false)
                            && st.get(sv) == Some(&VState::Owned)
                            && !self.loop_copy.contains(sv)
                            && !self.no_move.contains(sv)
                    });
                let mut arm_plans: Vec<Option<Vec<String>>> = Vec::with_capacity(arms.len());
                let mut arm_sts: Vec<HashMap<String, VState>> = Vec::with_capacity(arms.len());
                let mut arm_escapeds: Vec<HashSet<String>> = Vec::with_capacity(arms.len());
                let mut any_arm_escape = false;
                for arm in arms.iter_mut() {
                    let mut arm_tys = post_tys.clone();
                    let mut arm_st = post_scrut.clone();
                    let mut arm_owns = post_owns.clone();
                    let arm_field_tys: Option<Vec<Ty>> = scrut_adt
                        .and_then(|i| self.adts.get(i as usize))
                        .and_then(|def| arm.variant.and_then(|v| def.variants.get(v as usize)))
                        .map(|f| f.1.clone());
                    for (k, b) in arm.binders.iter().enumerate() {
                        // Boxed-scrutinee binders carry their real field
                        // type so moves through them register (they alias
                        // the scrutinee's box); the I64 stand-in remains
                        // for inline scrutinees, whose binders are copies
                        // with no memory stake.
                        // Move-registration only for fields whose binder
                        // aliases the scrutinee's heap (vec/map/set/boxed
                        // children). Str fields are Copy in this model
                        // (each use re-strdups at its store) and inline
                        // children copy whole, so their binders keep the
                        // unrestricted I64 stand-in.
                        let fty = match scrut_boxed
                            .then_some(arm_field_tys.as_ref())
                            .flatten()
                            .and_then(|f| f.get(k).copied())
                        {
                            Some(t @ (Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Adt(_)))
                                if binder_field_aliases(Some(t), &self.boxed_full) =>
                            {
                                t
                            }
                            _ => Ty::I64,
                        };
                        arm_tys.insert(b.clone(), fty);
                        arm_st.insert(b.clone(), VState::Owned);
                        arm_owns.insert(b.clone(), false);
                    }
                    // S9: while the arm body runs, watch for a move of the
                    // scrutinee var itself. The backend loads aliasing
                    // binders (str/vec/ADT fields) as pointers into or
                    // owned-by the scrutinee's one heap box, and the move
                    // hands that box to a callee that frees it — every
                    // aliasing-binder use after the move is a UAF
                    // (call-user's `mdl` reads after `call-marshal`).
                    let mut watched = false;
                    if scrut_boxed {
                        if let HExpr::Var(sv) = &**scrut {
                            if let Some(fields) = arm_field_tys.as_ref() {
                                let mut binders = HashSet::new();
                                for (k, b) in arm.binders.iter().enumerate() {
                                    let fty = fields.get(k).copied();
                                    let aliases = binder_field_aliases(fty, &self.boxed_full)
                                        || matches!(fty, Some(Ty::Adt(_)));
                                    if aliases {
                                        binders.insert(b.clone());
                                    }
                                }
                                if !binders.is_empty() {
                                    self.watches.push(Watch {
                                        scrutinee: sv.clone(),
                                        binders,
                                    });
                                    watched = true;
                                }
                            }
                        }
                    }
                    // arm binder types: recover field types are not carried;
                    // binders alias the scrutinee's fields, so they never
                    // own (I64 is the historical state-tracking stand-in).
                    // While the body runs, its move-trackable binders are
                    // known as aliases of a box this match frees (or not),
                    // for the branch-local frees of inner `if`/`match`.
                    let freeable = temp_fresh || var_owned;
                    let mut saved_alias: Vec<(String, Option<bool>)> = Vec::new();
                    for b in arm.binders.iter() {
                        if arm_tys.get(b).map(|t| is_non_copy(*t)).unwrap_or(false) {
                            saved_alias.push((b.clone(), self.freeable_alias.get(b).copied()));
                            self.freeable_alias.insert(b.clone(), freeable);
                        }
                    }
                    self.check(&mut arm.body, &mut arm_tys, &mut arm_st, &mut arm_owns);
                    for (b, prev) in saved_alias.into_iter().rev() {
                        match prev {
                            Some(v) => {
                                self.freeable_alias.insert(b, v);
                            }
                            None => {
                                self.freeable_alias.remove(&b);
                            }
                        }
                    }
                    if watched {
                        self.watches.pop();
                    }
                    // Per-arm escape set: binder names repeat across arms
                    // with different field types, so the defn-wide set
                    // would blame an alias arm for a copy arm's return.
                    let mut arm_escaped = HashSet::new();
                    tail_escape(&arm.body, &mut arm_escaped);
                    let mut any_alias_escape = false;
                    if scrut_boxed {
                        if let Some(fields) = arm_field_tys.as_ref() {
                            for (k, b) in arm.binders.iter().enumerate() {
                                let fty = fields.get(k).copied();
                                // Boxed-ADT children, vec/map/set and str
                                // fields alias the box's heap. An inline-ADT
                                // binder does not copy the value either: the
                                // backend materializes it as the field's
                                // address inside the box, so a binder that
                                // escapes (a bare-Var return) aliases too
                                // (S8 — the UAF behind ast-at's zeroed spans).
                                let aliases = binder_field_aliases(fty, &self.boxed_full)
                                    || matches!(fty, Some(Ty::Adt(_)));
                                if !aliases {
                                    continue;
                                }
                                if arm_escaped.contains(b)
                                    || arm_st.get(b) == Some(&VState::Moved)
                                {
                                    any_alias_escape = true;
                                    if scrut_borrowed_param {
                                        // A borrowed-returning (`&T`) defn
                                        // legitimately hands the aliasing
                                        // binder back to the caller: that is
                                        // the borrow.
                                        if arm_escaped.contains(b) && !self.ret_borrowed {
                                            let site = self.site_for(b);
                                            self.err(
                                                format!(
                                                    "cannot return {}: it aliases a field of the matched value — clone it first",
                                                    b
                                                ),
                                                Some(site),
                                            );
                                        }
                                        if arm_st.get(b) == Some(&VState::Moved) {
                                            let site = self.moved_at.get(b).cloned();
                                            self.err(
                                                format!(
                                                    "cannot move {}: it aliases a field of the matched value — clone it first",
                                                    b
                                                ),
                                                site,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        // An alias left the arm: the box's drop duty
                        // transfers to its new holder, so the scrutinee's
                        // own scope free (let drop or param free) is
                        // suppressed — the arm-exit free below takes over
                        // for the fields that stayed. Loop variables never
                        // free anyway.
                        if any_alias_escape {
                            if let HExpr::Var(sv) = &**scrut {
                                if !self.loop_copy.contains(sv) && !scrut_borrowed_param {
                                    self.move_var(sv, st, owns);
                                }
                            }
                        }
                    }
                    any_arm_escape |= any_alias_escape;
                    let plan = if temp_fresh || var_owned {
                        self.arm_exit_plan(
                            arm,
                            sv_name.as_deref(),
                            arm_field_tys.as_deref(),
                            &arm_st,
                            &arm_escaped,
                        )
                    } else if !scrut_boxed {
                        Err("not-boxed")
                    } else if sv_name.is_some() {
                        Err("var-not-owned")
                    } else {
                        Err("temp-not-fresh")
                    };
                    arm_plans.push(plan.ok());
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
                    arm_sts.push(arm_st);
                    arm_escapeds.push(arm_escaped);
                }
                // Arms are branches too: a pre-match binding moved in one
                // arm is freed by the arms that kept it.
                {
                    let sts: Vec<&HashMap<String, VState>> = arm_sts.iter().collect();
                    let shadow_owned: Vec<Vec<String>> = arms.iter().map(|a| a.binders.clone()).collect();
                    let shadow: Vec<&[String]> = shadow_owned.iter().map(|v| v.as_slice()).collect();
                    let mut bodies: Vec<&mut HExpr> = arms.iter_mut().map(|a| &mut a.body).collect();
                    self.branch_frees(&post_scrut, &post_owns, tys, &mut bodies, &sts, &shadow, sv_name.as_deref());
                }
                // A fresh temp frees here, always. A named box frees here
                // only when some arm transferred an alias out (its scope
                // free was just suppressed); otherwise it keeps that scope
                // free and stays readable after the match.
                let free_here = (temp_fresh || (var_owned && any_arm_escape))
                    && !flag("CATO_NO_ARMFREE");
                for ((arm, plan), esc) in arms.iter_mut().zip(arm_plans).zip(arm_escapeds.iter()) {
                    arm.exit_free = if free_here { plan } else { None };
                    // A binder that is the arm's value, with its slot skipped
                    // by the arm-exit free, hands its object to whoever
                    // receives the match: it counts as moved, so a `let`
                    // bound to this match owns the merge (and frees it).
                    // Not when the name also belongs to an outer binding
                    // the mark would poison.
                    if let Some(skip) = &arm.exit_free {
                        for b in skip {
                            if flag("CATO_NO_ESCTRANSFER") { break; }
                            if esc.contains(b) && !post_scrut.contains_key(b) {
                                st.insert(b.clone(), VState::Moved);
                            }
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
                let pre_st = st.clone();
                let pre_owns = owns.clone();
                let mut t_tys = tys.clone();
                let mut t_st = st.clone();
                let mut t_owns = owns.clone();
                self.check(t, &mut t_tys, &mut t_st, &mut t_owns);
                let mut e_tys = tys.clone();
                let mut e_st = st.clone();
                let mut e_owns = owns.clone();
                self.check(e2, &mut e_tys, &mut e_st, &mut e_owns);
                {
                    let sts = [&t_st, &e_st];
                    let mut bodies: Vec<&mut HExpr> = vec![&mut **t, &mut **e2];
                    self.branch_frees(&pre_st, &pre_owns, tys, &mut bodies, &sts, &[], None);
                }
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
                    // Owning control-flow merges (L-a): an `if`/`match`
                    // result of heap-owning type leaks today because
                    // `allocates` cannot see through the merge. At a bind
                    // site the duty has a receiver: arm tails that own a
                    // heap buffer move into the binding, and the binding
                    // owns the merge exactly when every arm yields owned
                    // duty (a fresh allocation, or a moved tail var). Arms
                    // that alias (binders of another box, non-owning
                    // aliases) do not move and keep the old behavior.
                    let merge_owns = self.transfer_merge_tails(value, tys, st, owns);
                    // Binding a non-Copy Var moves the buffer: the drop
                    // duty transfers to the new binding. Read the source's
                    // ownership before the move clears it.
                    let transferred = if let HExpr::Var(v) = &*value {
                        // A non-Copy loop variable rides as Copy: the let
                        // aliases the loop's slot, it does not consume it.
                        if is_non_copy(*ty) && !self.loop_copy.contains(v) {
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
                            // A borrowed (`&T`) call result is a borrow the
                            // caller still owns: never free it here.
                            None if matches!(value, HExpr::Call(callee, _)
                                if self.ret_borrowed_names.contains(callee)) =>
                            {
                                false
                            }
                            // An if/match merge whose every arm yielded
                            // owned duty: the binding owns the winning
                            // arm's buffer and frees it at scope exit.
                            None if merge_owns => true,
                            None => self.allocates_fresh(value),
                        },
                    );
                    tys.insert(name.clone(), *ty);
                    st.insert(name.clone(), VState::Owned);
                }
                self.check(body, tys, st, owns);
                // Scope exit: every binding that still owns a heap buffer
                // and was not moved, consumed, returned, or shadowed frees
                // here. This is the whole destruction rule.
                // A name re-bound within this let (`[a x a (conj a y)]`)
                // is checked once, for its last binding: the state maps
                // describe only that binding, and a second entry would free
                // the same buffer twice. The shadowed buffer is not freed
                // (a leak, the same contract as `rebound_in_body`).
                let mut out: Vec<String> = Vec::new();
                for (i, (name, _, ty)) in binds.iter().enumerate() {
                    if binds[i + 1..].iter().any(|(n, _, _)| n == name) {
                        continue;
                    }
                    if heap_owning(*ty, self.facts)
                        && owns.get(name).copied().unwrap_or(false)
                        && st.get(name) == Some(&VState::Owned)
                        && !self.escaped.contains(name)
                        && !self.consumed.contains(name)
                        // Suppressed only when this very body rebinds the
                        // name (the one shape where slot sharing is real).
                        // Sibling-branch lets of the same name are distinct
                        // bindings in distinct slots: each frees its own.
                        && !rebound_in_body(body, name)
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
            // Fused loop stages run per iteration, with the same rule as
            // `for`: the body is checked against the live state, and moves
            // of bindings that predate the loop are rejected (the second
            // iteration would read a moved value). Stage binders (`el`,
            // `acc`) are per-iteration; their types are not carried on the
            // node, so they keep the I64 stand-in (a reduce accumulator of
            // move type stays untracked — the pre-existing limitation).
            HExpr::Reduce(r) => {
                let (init, src, maps, filters, closure_body, acc, el) = match &mut **r {
                    HReduce::Pipeline {
                        init,
                        src,
                        maps,
                        filters,
                        ..
                    } => (init, src, maps, filters, None, None, None),
                    HReduce::Closure {
                        init,
                        src,
                        maps,
                        filters,
                        acc,
                        el,
                        body,
                        ..
                    } => (
                        init,
                        src,
                        maps,
                        filters,
                        Some(body),
                        Some(acc),
                        Some(el),
                    ),
                };
                let pre: HashSet<String> = st.keys().cloned().collect();
                let pre_moved: HashSet<String> = st
                    .iter()
                    .filter(|(_, s)| **s == VState::Moved)
                    .map(|(k, _)| k.clone())
                    .collect();
                self.check(init, tys, st, owns);
                self.check_source(src, tys, st, owns);
                for m in maps.iter_mut() {
                    if let Some(e2) = &m.el2 {
                        tys.insert(e2.clone(), m.el2_ty);
                        st.insert(e2.clone(), VState::Owned);
                    }
                    tys.insert(m.el.clone(), m.el_ty);
                    st.insert(m.el.clone(), VState::Owned);
                    self.check(&mut m.body, tys, st, owns);
                }
                for f in filters.iter_mut() {
                    if let Some(e2) = &f.el2 {
                        tys.insert(e2.clone(), f.el2_ty);
                        st.insert(e2.clone(), VState::Owned);
                    }
                    tys.insert(f.el.clone(), f.el_ty);
                    st.insert(f.el.clone(), VState::Owned);
                    self.check(&mut f.body, tys, st, owns);
                }
                if let (Some(body), Some(acc), Some(el)) = (closure_body, acc, el) {
                    tys.insert(acc.clone(), Ty::I64);
                    st.insert(acc.clone(), VState::Owned);
                    tys.insert(el.clone(), Ty::I64);
                    st.insert(el.clone(), VState::Owned);
                    self.check(body, tys, st, owns);
                }
                self.forbid_pred_moves(&pre, &pre_moved, st, tys, "a fused loop");
            }
            HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::VecLit(_) => {}
        }
    }

    /// Reads of a fused-loop source: `Var` sources are plain reads (the
    /// loop does not consume its vector); bounds and nesting recurse.
    fn check_source(
        &mut self,
        src: &mut HSource,
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        match src {
            HSource::Var(v) => self.use_var(v, tys, st),
            HSource::Lit(_) => {}
            HSource::Range(hi, _) => self.check(hi, tys, st, owns),
            HSource::Zip(a, b) => {
                self.check_source(a, tys, st, owns);
                self.check_source(b, tys, st, owns);
            }
            HSource::Take(n, inner) | HSource::Drop(n, inner) => {
                self.check(n, tys, st, owns);
                self.check_source(inner, tys, st, owns);
            }
        }
    }

    /// Per-iteration bodies (`for`, fused loops) may move their own fresh
    /// bindings, but a move of a binding that existed before the loop frees
    /// a buffer the second iteration still reads. Anything that went Moved
    /// during the walk and was bound before it is an error.
    fn forbid_pred_moves(
        &mut self,
        pre: &HashSet<String>,
        pre_moved: &HashSet<String>,
        st: &HashMap<String, VState>,
        tys: &HashMap<String, Ty>,
        what: &str,
    ) {
        let mut reported: HashSet<String> = HashSet::new();
        for (k, v) in st {
            // Moved *during the walk* (a state transition), on a binding
            // that predates the loop: st is live, so pre-existing moves
            // must not be re-flagged here.
            if *v == VState::Moved
                && pre.contains(k)
                && !pre_moved.contains(k)
                && !self.loop_copy.contains(k)
                && tys.get(k).map(|t| is_non_copy(*t)).unwrap_or(false)
                && reported.insert(k.clone())
            {
                let site = self.moved_at.get(k).cloned();
                self.err(
                    format!("cannot move {} inside {}: the body runs once per element", k, what),
                    site,
                );
            }
        }
    }

    /// Does this if/match result of heap-owning type have a receiver for
    /// the arms' drop duty? If so, move every owning arm-tail var into the
    /// coming binding (the same transfer a plain let-of-var performs) and
    /// report whether all arms yield owned duty. Arm tails that only alias
    /// (binder of another box, non-owning alias) do not move: the binding
    /// keeps the old not-owning behavior for that merge.
    fn transfer_merge_tails(
        &mut self,
        value: &HExpr,
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) -> bool {
        match value {
            HExpr::If(_, then, els, ty) if heap_owning(*ty, self.facts) => {
                self.move_tail(then, tys, st, owns);
                self.move_tail(els, tys, st, owns);
                self.tail_yields_owned(then, st) && self.tail_yields_owned(els, st)
            }
            HExpr::Match { arms, ty, .. } if heap_owning(*ty, self.facts) => {
                for arm in arms {
                    self.move_tail(&arm.body, tys, st, owns);
                }
                arms.iter().all(|arm| self.tail_yields_owned(&arm.body, st))
            }
            _ => false,
        }
    }

    /// Move the heap-owning vars in tail position into the enclosing
    /// result. Only vars that currently *own* their buffer move: aliases
    /// and match binders (owns=false) stay put — their buffers belong to
    /// someone else, and moving them here would hand the same buffer to
    /// two owners.
    fn move_tail(
        &mut self,
        e: &HExpr,
        tys: &mut HashMap<String, Ty>,
        st: &mut HashMap<String, VState>,
        owns: &mut HashMap<String, bool>,
    ) {
        match e {
            HExpr::Var(v) => {
                if owns.get(v).copied().unwrap_or(false)
                    && st.get(v) == Some(&VState::Owned)
                    && tys.get(v).map(|t| is_non_copy(*t)).unwrap_or(false)
                    && !self.loop_copy.contains(v)
                {
                    self.move_var(v, st, owns);
                }
            }
            HExpr::Let(_, body, _) => self.move_tail(body, tys, st, owns),
            HExpr::Seq(vs) => {
                if let Some(last) = vs.last() {
                    self.move_tail(last, tys, st, owns);
                }
            }
            HExpr::If(_, then, els, _) => {
                self.move_tail(then, tys, st, owns);
                self.move_tail(els, tys, st, owns);
            }
            HExpr::Match { arms, .. } => {
                for arm in arms {
                    self.move_tail(&arm.body, tys, st, owns);
                }
            }
            _ => {}
        }
    }

    /// Does this tail hand the merge a buffer it uniquely owns? A tail var
    /// counts when it was moved by `move_tail` (duty transferred); anything
    /// else counts when it allocates fresh.
    fn tail_yields_owned(&self, e: &HExpr, st: &HashMap<String, VState>) -> bool {
        match e {
            HExpr::Var(v) => st.get(v) == Some(&VState::Moved),
            HExpr::Let(_, body, _) => self.tail_yields_owned(body, st),
            HExpr::Seq(vs) => vs
                .last()
                .map(|last| self.tail_yields_owned(last, st))
                .unwrap_or(false),
            HExpr::If(_, then, els, _) => {
                self.tail_yields_owned(then, st) && self.tail_yields_owned(els, st)
            }
            HExpr::Match { arms, .. } => arms
                .iter()
                .all(|arm| self.tail_yields_owned(&arm.body, st)),
            // A loop yields owned duty when every completing value is a
            // fresh allocation or a loop variable whose seed allocated:
            // such a slot always holds a heap buffer (fresh, or regrown).
            HExpr::Loop { binds, body, .. } => {
                if flag("CATO_NO_LOOPOWN") {
                    return false;
                }
                let mut leaves = Vec::new();
                tail_leaves(body, &mut leaves);
                leaves.iter().all(|l| match l {
                    HExpr::Recur(_) => true,
                    HExpr::Var(v) => binds
                        .iter()
                        .any(|(n, _, init)| n == v && self.allocates_fresh(init)),
                    other => self.allocates_fresh(other),
                })
            }
            other => self.allocates_fresh(other),
        }
    }

    /// `allocates`, minus the calls whose string/collection result may be
    /// a parameter's own buffer (never fresh for the caller).
    fn allocates_fresh(&self, e: &HExpr) -> bool {
        match e {
            HExpr::Call(c, _) => !self.alias_ret.contains(c) && !self.ret_borrowed_names.contains(c),
            HExpr::Let(_, body, _) => self.allocates_fresh(body),
            other => allocates(other, self.facts),
        }
    }

    /// The static type of a fresh heap value, if `e` is one the caller would
    /// otherwise lose: a boxed box (`boxed_only`), or any string/collection
    /// temp — an owned call result, a boxed constructor, a clone, a string
    /// or collection builder. Calls through `alias_calls` are not fresh.
    fn fresh_temp_ty(&self, e: &HExpr, tys: &HashMap<String, Ty>, boxed_only: bool) -> Option<Ty> {
        let boxed = |t: Ty| matches!(t, Ty::Adt(i) if self.boxed_full.get(i as usize).copied().unwrap_or(false));
        let coll = |t: Ty| matches!(t, Ty::Str | Ty::Vec(_) | Ty::Map | Ty::Set);
        let want = |t: Ty| if boxed_only { boxed(t) } else { coll(t) };
        match e {
            HExpr::Call(c, _) if !self.alias_calls.contains(c) => {
                self.rets.get(c).copied().filter(|t| want(*t))
            }
            HExpr::Ctor { adt, .. } if boxed_only && boxed(Ty::Adt(*adt)) => Some(Ty::Adt(*adt)),
            HExpr::Clone(inner) => match &**inner {
                HExpr::Var(v) => tys.get(v).copied().filter(|t| want(*t)),
                other => self.fresh_temp_ty(other, tys, boxed_only),
            },
            HExpr::Subs { .. }
            | HExpr::StrCat(_)
            | HExpr::Join { .. }
            | HExpr::ToStr(..)
            | HExpr::CharStr(_)
            | HExpr::Slurp(_)
                if !boxed_only =>
            {
                Some(Ty::Str)
            }
            HExpr::Split { .. } if !boxed_only => Some(Ty::Vec(Elem::Str)),
            HExpr::Conj { elem, .. } | HExpr::Subvec { elem, .. } if !boxed_only => Some(Ty::Vec(*elem)),
            HExpr::MapLit(_) | HExpr::MapAssoc { .. } if !boxed_only => Some(Ty::Map),
            HExpr::SetLit(_) | HExpr::SetConj { .. } | HExpr::SetDisj { .. } if !boxed_only => Some(Ty::Set),
            _ => None,
        }
    }

    /// Can this argument stay in place when a later argument is hoisted
    /// before it? Reads of bindings, literals and arithmetic over them have
    /// no effects to reorder.
    fn order_free(e: &HExpr) -> bool {
        match e {
            HExpr::Var(_)
            | HExpr::StrLit(_)
            | HExpr::ConstI64(_)
            | HExpr::ConstF64(_)
            | HExpr::ConstB(_)
            | HExpr::VecLit(_)
            | HExpr::Args => true,
            HExpr::BinW(_, a, b, _) | HExpr::CmpW(_, a, b, _) | HExpr::BinF(_, a, b) => {
                Self::order_free(a) && Self::order_free(b)
            }
            HExpr::And(a, b) | HExpr::Or(a, b) | HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
                Self::order_free(a) && Self::order_free(b)
            }
            HExpr::Not(a) | HExpr::Abs(a, _) | HExpr::ICast(a, _, _) | HExpr::Count(a) => {
                Self::order_free(a)
            }
            HExpr::StrCmp(_, a, b) | HExpr::StrOrd(_, a, b) => Self::order_free(a) && Self::order_free(b),
            HExpr::StrNth { s, idx, res: None } => Self::order_free(s) && Self::order_free(idx),
            _ => false,
        }
    }

    /// The static type to bind a hoisted argument under, when known.
    fn hoist_ty(&self, e: &HExpr, tys: &HashMap<String, Ty>) -> Option<Ty> {
        match e {
            HExpr::Call(c, _) => self.rets.get(c).copied(),
            HExpr::Ctor { adt, .. } => Some(Ty::Adt(*adt)),
            HExpr::Clone(inner) => match &**inner {
                HExpr::Var(v) => tys.get(v).copied(),
                other => self.hoist_ty(other, tys),
            },
            HExpr::Subs { .. }
            | HExpr::StrCat(_)
            | HExpr::Join { .. }
            | HExpr::ToStr(..)
            | HExpr::CharStr(_)
            | HExpr::Slurp(_) => Some(Ty::Str),
            HExpr::Split { .. } => Some(Ty::Vec(Elem::Str)),
            HExpr::Conj { elem, .. } | HExpr::Subvec { elem, .. } => Some(Ty::Vec(*elem)),
            HExpr::MapLit(_) | HExpr::MapAssoc { .. } => Some(Ty::Map),
            HExpr::SetLit(_) | HExpr::SetConj { .. } | HExpr::SetDisj { .. } => Some(Ty::Set),
            HExpr::If(_, _, _, ty) | HExpr::Match { ty, .. } | HExpr::Loop { ty, .. } => Some(*ty),
            HExpr::Get { ty, res: None, .. } => Some(*ty),
            HExpr::Get { res: Some(t), .. } | HExpr::StrNth { res: Some(t), .. } => Some(*t),
            HExpr::ParseNum { res, .. } => Some(*res),
            _ => None,
        }
    }

    /// Hoist the argument temps a node would otherwise leak into a `let`
    /// around it — `(f … <temp> …)` becomes `(let [~aN <temp>] (f … ~aN …))`
    /// and the ordinary `let` planning frees `~aN` after the node. Two
    /// kinds leak: a fresh box on a borrowed parameter (the callee only
    /// reads it, nobody owns it), and any fresh string/collection temp
    /// (callees never free those; constructors, `str`, `conj` and `spit`
    /// copy or consume the bytes). Not under a `&T` or alias-returning
    /// callee, whose result may be the temp itself. Evaluation order is
    /// kept: every effectful argument before a hoisted one is hoisted as
    /// well (under its own type), and the hoist stops at an argument it
    /// cannot bind.
    fn hoist_arg_temps(&mut self, e: &mut HExpr, tys: &HashMap<String, Ty>) {
        let (args_view, kinds): (Vec<&HExpr>, Vec<Borrow>) = match e {
            HExpr::Call(name, args) => {
                if self.alias_calls.contains(name) {
                    return;
                }
                let kinds = self.kinds_of(name, args.len());
                (args.iter().collect(), kinds)
            }
            HExpr::StrCat(args) | HExpr::Ctor { args, .. } => {
                (args.iter().collect(), vec![Borrow::Owned; args.len()])
            }
            HExpr::Conj { x, .. } => (vec![&**x], vec![Borrow::Owned]),
            HExpr::Spit { path, data } => (vec![&**path, &**data], vec![Borrow::Owned; 2]),
            _ => return,
        };
        let mut plan: Vec<(usize, Ty)> = Vec::new();
        let mut worth = false;
        for (i, (a, k)) in args_view.iter().zip(kinds.iter()).enumerate() {
            if Self::order_free(a) {
                continue;
            }
            let leaks = self.fresh_temp_ty(a, tys, false).is_some()
                || (*k != Borrow::Owned && self.fresh_temp_ty(a, tys, true).is_some());
            match self.hoist_ty(a, tys) {
                Some(t) => {
                    plan.push((i, t));
                    worth |= leaks;
                }
                None => break,
            }
        }
        // Trailing hoists that free nothing only add noise: keep the plan
        // up to the last leaking argument.
        while let Some((i, _)) = plan.last() {
            let a = args_view[*i];
            let k = kinds[*i];
            let leaks = self.fresh_temp_ty(a, tys, false).is_some()
                || (k != Borrow::Owned && self.fresh_temp_ty(a, tys, true).is_some());
            if leaks {
                break;
            }
            plan.pop();
        }
        if !worth || plan.is_empty() {
            return;
        }
        let mut binds = Vec::new();
        {
            let counter = &mut self.tmp_counter;
            let mut slot = |child: &mut HExpr, ty: Ty| {
                let tmp = format!("~a{}", *counter);
                *counter += 1;
                let value = std::mem::replace(child, HExpr::Var(tmp.clone()));
                binds.push((tmp, value, ty));
            };
            match e {
                HExpr::Call(_, args) | HExpr::StrCat(args) | HExpr::Ctor { args, .. } => {
                    for (i, t) in plan {
                        slot(&mut args[i], t);
                    }
                }
                HExpr::Conj { x, .. } => {
                    for (_, t) in plan {
                        slot(x, t);
                    }
                }
                HExpr::Spit { path, data } => {
                    for (i, t) in plan {
                        slot(if i == 0 { path } else { data }, t);
                    }
                }
                _ => {}
            }
        }
        let taken = std::mem::replace(e, HExpr::ConstI64(0));
        *e = HExpr::Let(binds, Box::new(taken), Vec::new());
    }

    /// Branch-local frees. A binding moved on some branches of an `if` or
    /// `match` and not on others would be consumed as a whole (no scope
    /// free anywhere) and leak on every path that kept it. Instead each
    /// non-moving branch frees it itself, through the empty-binds `Let`
    /// wrapper the backends already emit. Eligible: a binding that owned
    /// its buffer before the branch, or an aliasing binder of a box the
    /// enclosing arm frees at exit (its slot is skipped there). A branch
    /// whose value is the binding, or that recurs, is left alone; so is a
    /// name the branch rebinds (`shadow`: that branch's own binders).
    fn branch_frees(
        &mut self,
        pre_st: &HashMap<String, VState>,
        pre_owns: &HashMap<String, bool>,
        tys: &HashMap<String, Ty>,
        bodies: &mut [&mut HExpr],
        sts: &[&HashMap<String, VState>],
        shadow: &[&[String]],
        exclude: Option<&str>,
    ) {
        if flag("CATO_NO_BRANCHFREE") {
            return;
        }
        let moved_in: Vec<HashSet<String>> = sts
            .iter()
            .map(|st_i| {
                st_i.iter()
                    .filter(|(n, v)| {
                        **v == VState::Moved && pre_st.get(*n) == Some(&VState::Owned)
                    })
                    .map(|(n, _)| n.clone())
                    .collect()
            })
            .collect();
        let mut union: Vec<String> = moved_in.iter().flatten().cloned().collect();
        union.sort();
        union.dedup();
        if union.is_empty() {
            return;
        }
        for (i, body) in bodies.iter_mut().enumerate() {
            if tail_has_recur(body) {
                continue;
            }
            let mut frees = Vec::new();
            for n in &union {
                if moved_in[i].contains(n) {
                    continue;
                }
                if shadow.get(i).is_some_and(|names| names.contains(n)) {
                    continue;
                }
                // A matched box belongs to its arms' exit frees: its binders
                // alias it, so a free here would dangle them (and double
                // the arm-exit free).
                if exclude == Some(n.as_str()) || self.watches.iter().any(|w| &w.scrutinee == n) {
                    continue;
                }
                // The branch's value may still reach into the binding.
                let mut one = HashSet::new();
                one.insert(n.clone());
                if tail_may_alias(body, &one, &self.alias_calls) {
                    continue;
                }
                let non_copy = tys.get(n).map(|t| is_non_copy(*t)).unwrap_or(false);
                let eligible = non_copy
                    && !self.loop_copy.contains(n)
                    && !self.no_move.contains(n)
                    && (pre_owns.get(n).copied().unwrap_or(false)
                        || self.freeable_alias.get(n).copied().unwrap_or(false));
                if eligible {
                    frees.push(n.clone());
                }
            }
            if !frees.is_empty() {
                let taken = std::mem::replace(*body, HExpr::ConstI64(0));
                **body = HExpr::Let(Vec::new(), Box::new(taken), frees);
            }
        }
    }

    /// One arm's exit free of a boxed scrutinee this match may release:
    /// the binders whose field slots are nulled before the glue runs, or
    /// `None` when the box cannot be freed at this arm's exit at all.
    fn arm_exit_plan(
        &self,
        arm: &HArm,
        sv: Option<&str>,
        fields: Option<&[Ty]>,
        arm_st: &HashMap<String, VState>,
        arm_escaped: &HashSet<String>,
    ) -> Result<Vec<String>, &'static str> {
        // The arm hands the box itself on (moves or returns it): its new
        // holder frees it.
        if let Some(sv) = sv {
            if arm_st.get(sv) == Some(&VState::Moved) || arm_escaped.contains(sv) {
                return Err("scrutinee-moved");
            }
        }
        // A recurring arm has no exit of its own: the loop head is next.
        if tail_has_recur(&arm.body) {
            return Err("recur");
        }
        let fields = match fields {
            Some(f) => f,
            // The wildcard arm binds nothing: the whole box goes.
            None => return Ok(Vec::new()),
        };
        // Binders that alias the box's heap. Boxed children, closures,
        // strings and collections point at separately allocated objects;
        // an inline child is the field's own address inside the box.
        let mut alias: HashSet<String> = HashSet::new();
        for (k, b) in arm.binders.iter().enumerate() {
            let fty = fields.get(k).copied();
            if binder_field_aliases(fty, &self.boxed_full) || matches!(fty, Some(Ty::Adt(_))) {
                alias.insert(b.clone());
            }
        }
        // May the arm's value point into the box's objects? Only the bare
        // alias-binder shape is precise enough to skip; a `&T` call over a
        // binder, a `let` alias, an element load keep the box alive instead.
        if tail_may_alias(&arm.body, &alias, &self.alias_calls) {
            let mut leaves = Vec::new();
            tail_leaves(&arm.body, &mut leaves);
            if !leaves
                .iter()
                .all(|l| matches!(l, HExpr::Var(v) if alias.contains(v)))
            {
                return Err("value-may-alias");
            }
        }
        let mut skip = Vec::new();
        // A string or collection handed to a callee that returns the same
        // kind may come straight back (`(defn f [s :- str] :- str s)`): the
        // caller then binds that result as owned and frees it, so the box
        // must not free the same buffer. Constructors copy, and a callee
        // returning anything else can only keep a copy.
        let mut through_call: HashSet<String> = HashSet::new();
        crate::opt::walk::visit_nodes(&arm.body, &mut |n| match n {
            HExpr::Call(name, args)
                if matches!(
                    self.rets.get(name),
                    Some(Ty::Str) | Some(Ty::Vec(_)) | Some(Ty::Map) | Some(Ty::Set)
                ) =>
            {
                for a in args {
                    if let HExpr::Var(v) = a {
                        through_call.insert(v.clone());
                    }
                }
            }
            HExpr::ClosureCall { args, .. } => {
                for a in args {
                    if let HExpr::Var(v) = a {
                        through_call.insert(v.clone());
                    }
                }
            }
            _ => {}
        });
        for (k, b) in arm.binders.iter().enumerate() {
            let fty = fields.get(k).copied();
            let escapes = arm_escaped.contains(b);
            let moved = arm_st.get(b) == Some(&VState::Moved);
            match fty {
                // A boxed child moved into a callee or constructor (which
                // now owns it) or returned: its slot must not be freed.
                Some(Ty::Adt(j)) if self.boxed_full.get(j as usize).copied().unwrap_or(false) => {
                    if moved || escapes {
                        skip.push(b.clone());
                    }
                }
                Some(Ty::Fn(_)) => {
                    if moved || escapes {
                        skip.push(b.clone());
                    }
                }
                // An inline child returned is the box's own memory.
                Some(Ty::Adt(_)) => {
                    if moved || escapes {
                        return Err("inline-escape");
                    }
                }
                // Strings and collections: callees never free them and
                // constructors copy them, so the box keeps its buffer
                // unless the binder itself is the arm's value or may come
                // back out of a call.
                Some(Ty::Str) | Some(Ty::Vec(_)) | Some(Ty::Map) | Some(Ty::Set) => {
                    if escapes || through_call.contains(b) {
                        skip.push(b.clone());
                    }
                }
                _ => {}
            }
        }
        Ok(skip)
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
            if let Some(d) = default.as_ref() { count_vars(d, counts) }
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
            if let Some(d) = default.as_ref() { count_vars(d, counts) }
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
        // A `let`'s value is its body's (the planner's hoisted-temp lets
        // wrap allocating calls).
        HExpr::Let(_, body, _) => allocates(body, facts),
        _ => false,
    }
}
