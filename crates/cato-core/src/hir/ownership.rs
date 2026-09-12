use super::*;

pub(crate) fn is_non_copy(ty: Ty) -> bool {
    matches!(ty, Ty::Adt(_) | Ty::Vec(_))
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
            HExpr::StrLit(_) | HExpr::StrCmp(..) => {}
            HExpr::StrCat(args) => {
                for a in args {
                    self.check(a, tys, st);
                }
            }
            HExpr::BinW(_, a, b, _) => {
                self.check(a, tys, st);
                self.check(b, tys, st);
            }
            HExpr::CmpW(_, a, b) => {
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
                for arm in arms {
                    let mut arm_tys = tys.clone();
                    let mut arm_st = st.clone();
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
            HExpr::Let(binds, body) => {
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

