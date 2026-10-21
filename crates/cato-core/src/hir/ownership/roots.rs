//! May-alias roots: which bindings a value may point into.
//!
//! A variable is its own root; `BCheck::roots` records what each binding in
//! scope may point into (a `let` of an alias, a match binder of a box, a
//! non-owning loop variable). Fresh values — string builders, clones,
//! owned call results, scalars — have no roots. The drop planner never
//! frees a binding that a surviving value may still reach.

use super::*;

impl<'a> BCheck<'a> {
    /// The may-alias roots of `e`'s value: every binding whose memory the
    /// value may point into (a variable is its own root; `roots` carries
    /// what each binding in scope may point into). Fresh values — string
    /// builders, clones, owned call results, scalars — have none. The walk
    /// is exhaustive over `HExpr`, so a new node kind must be classified.
    pub(super) fn roots_of(&self, e: &HExpr) -> HashSet<String> {
        let mut out = HashSet::new();
        self.roots_into(e, &HashMap::new(), &mut out);
        out
    }

    pub(super) fn roots_into(
        &self,
        e: &HExpr,
        extra: &HashMap<String, HashSet<String>>,
        out: &mut HashSet<String>,
    ) {
        let var = |v: &String, out: &mut HashSet<String>| {
            out.insert(v.clone());
            if let Some(r) = extra.get(v).or_else(|| self.roots.get(v)) {
                out.extend(r.iter().cloned());
            }
        };
        match e {
            HExpr::Var(v) => var(v, out),
            HExpr::ConstI64(_)
            | HExpr::ConstF64(_)
            | HExpr::ConstB(_)
            | HExpr::StrLit(_)
            | HExpr::VecLit(_)
            | HExpr::Count(_)
            | HExpr::BinW(..)
            | HExpr::BinF(..)
            | HExpr::CmpW(..)
            | HExpr::ICast(..)
            | HExpr::StrCmp(..)
            | HExpr::StrOrd(..)
            | HExpr::StrNth { .. }
            | HExpr::SetContains { .. }
            | HExpr::And(..)
            | HExpr::Or(..)
            | HExpr::Not(_)
            | HExpr::Abs(..)
            | HExpr::Min(..)
            | HExpr::Max(..)
            | HExpr::StrCat(_)
            | HExpr::Subs { .. }
            | HExpr::Split { .. }
            | HExpr::Join { .. }
            | HExpr::ToStr(..)
            | HExpr::CharStr(_)
            | HExpr::Slurp(_)
            | HExpr::Spit { .. }
            | HExpr::Args
            | HExpr::Exit(_)
            | HExpr::Clone(_)
            | HExpr::ParseNum { .. }
            | HExpr::MapLit(_)
            | HExpr::SetLit(_)
            | HExpr::Recur(..)
            | HExpr::DropStmt(_)
            | HExpr::Reduce(_) => {}
            HExpr::Call(f, args) => {
                if self.alias_calls.contains(f) {
                    for a in args {
                        self.roots_into(a, extra, out);
                    }
                }
            }
            HExpr::Ctor { args, .. } => {
                for a in args {
                    self.roots_into(a, extra, out);
                }
            }
            HExpr::Conj { v, .. } | HExpr::Subvec { v, .. } => self.roots_into(v, extra, out),
            HExpr::MapAssoc { m, .. } => self.roots_into(m, extra, out),
            HExpr::SetConj { s, .. } | HExpr::SetDisj { s, .. } => self.roots_into(s, extra, out),
            HExpr::Get { src, default, .. } | HExpr::MapGet { src, default, .. } => {
                self.roots_into(src, extra, out);
                if let Some(d) = default {
                    self.roots_into(d, extra, out);
                }
            }
            HExpr::Let(binds, body, _, _) => {
                let mut ex = extra.clone();
                for (n, v, _) in binds {
                    let mut r = HashSet::new();
                    self.roots_into(v, &ex, &mut r);
                    ex.insert(n.clone(), r);
                }
                self.roots_into(body, &ex, out);
            }
            HExpr::Seq(vs) => {
                if let Some(l) = vs.last() {
                    self.roots_into(l, extra, out);
                }
            }
            HExpr::If(_, t, f, _) => {
                self.roots_into(t, extra, out);
                self.roots_into(f, extra, out);
            }
            HExpr::Match { scrut, arms, .. } => {
                let mut r = HashSet::new();
                self.roots_into(scrut, extra, &mut r);
                for arm in arms {
                    let mut ex = extra.clone();
                    for b in &arm.binders {
                        ex.insert(b.clone(), r.clone());
                    }
                    self.roots_into(&arm.body, &ex, out);
                }
            }
            HExpr::Loop { binds, body, .. } => {
                let mut ex = extra.clone();
                for (n, _, init) in binds {
                    let mut r = HashSet::new();
                    self.roots_into(init, &ex, &mut r);
                    out.extend(r.iter().cloned());
                    ex.insert(n.clone(), r);
                }
                self.roots_into(body, &ex, out);
            }
            HExpr::Closure { captures, .. } => {
                // A str capture is the env's own copy (the backend strdups
                // it), so the closure does not point into the source binding:
                // that binding keeps its duty and is freed at scope exit.
                // Non-Copy captures move into the env (their source loses
                // `owns`), so rooting them here changes nothing.
                for (n, ty) in captures {
                    if *ty != Ty::Str {
                        var(n, out);
                    }
                }
            }
            HExpr::ClosureCall { args, .. } => {
                // Calling a closure reads the callee binding (a borrow): the
                // result never points into the closure *value* itself, so the
                // callee is not a root and keeps its scope-exit free. A result
                // that aliases a capture is the closure body's concern.
                for a in args {
                    self.roots_into(a, extra, out);
                }
            }
            HExpr::For { src, body, .. } => {
                self.roots_into(src, extra, out);
                self.roots_into(body, extra, out);
            }
        }
    }
}
