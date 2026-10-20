use super::*;

pub(crate) fn has_ctl_source(src: &HSource) -> bool {
    match src {
        HSource::Var(_) | HSource::Lit(_) => false,
        HSource::Range(a, b) => has_ctl(a) || has_ctl(b),
        HSource::Zip(a, b) => has_ctl_source(a) || has_ctl_source(b),
        HSource::Take(n, inner) | HSource::Drop(n, inner) => has_ctl(n) || has_ctl_source(inner),
    }
}

/// Does this expression (or a stage / closure body) fork into basic blocks?
pub(crate) fn has_ctl(e: &HExpr) -> bool {
    match e {
        HExpr::If(..) | HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Match { .. } => {
            true
        }
        HExpr::StrLit(_) => false,
        HExpr::StrCmp(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::StrOrd(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::StrNth { s, idx, .. } => has_ctl(s) || has_ctl(idx),
        HExpr::CharStr(c) => has_ctl(c),
        HExpr::Loop { binds, body, .. } => {
            binds.iter().any(|(_, _, e2)| has_ctl(e2)) || has_ctl(body)
        }
        HExpr::Recur(args, _) => args.iter().any(has_ctl),
        HExpr::ToStr(e, _) => has_ctl(e),
        HExpr::ParseNum { s, .. } => has_ctl(s),
        HExpr::Slurp(p) => has_ctl(p),
        HExpr::Spit { path, data } => has_ctl(path) || has_ctl(data),
        HExpr::Args => false,
        HExpr::Exit(c) => has_ctl(c),
        HExpr::BinW(_, a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::CmpW(_, a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::BinF(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::ICast(inner, _, _) => has_ctl(inner),
        HExpr::StrCat(args) => args.iter().any(has_ctl),
        HExpr::MapLit(pairs) => pairs
            .iter()
            .any(|(k, v)| has_ctl(k) || has_ctl(v)),
        HExpr::MapAssoc { m, k, v } => has_ctl(m) || has_ctl(k) || has_ctl(v),
        HExpr::MapGet { src, key, default, res } => {
            has_ctl(src)
                || has_ctl(key)
                || default.as_deref().map(has_ctl).unwrap_or(false)
        }
        HExpr::SetLit(vs) => vs.iter().any(has_ctl),
        HExpr::SetConj { s, x } | HExpr::SetDisj { s, x } | HExpr::SetContains { s, x } => {
            has_ctl(s) || has_ctl(x)
        }
        HExpr::Subs { s, start, end } => has_ctl(s) || has_ctl(start) || has_ctl(end),
        HExpr::Split { s, sep } => has_ctl(s) || has_ctl(sep),
        HExpr::Join { sep, xs } => has_ctl(sep) || has_ctl(xs),
        HExpr::Closure { body, .. } => has_ctl(body),
        HExpr::ClosureCall { f, args, .. } => has_ctl(f) || args.iter().any(has_ctl),
        HExpr::Conj { .. } => true, // growth branches
        HExpr::Subvec { v, start, end, .. } => has_ctl(v) || has_ctl(start) || has_ctl(end),
        HExpr::For { .. } => true, // fill loop
        HExpr::Count(x) | HExpr::Abs(x, _) => has_ctl(x),
        HExpr::Clone(x) | HExpr::DropStmt(x) => has_ctl(x),
        HExpr::Call(_, args) => args.iter().any(has_ctl),
        HExpr::Ctor { args, .. } => args.iter().any(has_ctl),
        HExpr::Get { idx, default, .. } => {
            has_ctl(idx) || default.as_deref().map(has_ctl).unwrap_or(false)
        }
        HExpr::Let(binds, body, _) => binds.iter().any(|(_, e, _)| has_ctl(e)) || has_ctl(body),
        HExpr::Seq(vs) => vs.iter().any(has_ctl),
        HExpr::Var(_) | HExpr::ConstI64(_) | HExpr::ConstB(_) | HExpr::ConstF64(_)
        | HExpr::VecLit(_) => false,
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline { init, maps, filters, src, .. } => {
                has_ctl(init)
                    || has_ctl_source(src)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
            HReduce::Closure { init, src, body, maps, filters, .. } => {
                has_ctl(init)
                    || has_ctl_source(src)
                    || has_ctl(body)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
        },
    }
}

/// Pin a bare literal into a virtual register before a `br`/`phi`.
pub(crate) fn collect_vars<F: FnMut(String)>(h: &HExpr, f: &mut F) {
    match h {
        HExpr::Var(v) => f(v.clone()),
        HExpr::Count(e) => collect_vars(e, f),
        HExpr::If(c, t, e, _) => {
            collect_vars(c, f);
            collect_vars(t, f);
            collect_vars(e, f);
        }
        HExpr::And(a, b) | HExpr::Or(a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Not(v) => collect_vars(v, f),
        HExpr::Abs(v, _) => collect_vars(v, f),
        HExpr::Clone(v) | HExpr::DropStmt(v) => collect_vars(v, f),
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline {
                init, maps, filters, src, ..
            } => {
                collect_vars(init, f);
                walk_source(src, f);
                for m in maps {
                    collect_vars(&m.body, f);
                }
                for fl in filters {
                    collect_vars(&fl.body, f);
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
                collect_vars(init, f);
                walk_source(src, f);
                for m in maps {
                    collect_vars(&m.body, f);
                }
                for fl in filters {
                    collect_vars(&fl.body, f);
                }
                collect_vars(body, f);
            }
        },
        HExpr::Get { idx, default, .. } => {
            collect_vars(idx, f);
            if let Some(d) = default {
                collect_vars(d, f);
            }
        }
        HExpr::Call(_, args) => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::Let(binds, body, _) => {
            for (_, e, _) in binds {
                collect_vars(e, f);
            }
            collect_vars(body, f);
        }
        HExpr::Seq(vs) => {
            for v in vs {
                collect_vars(v, f);
            }
        }
        HExpr::ConstI64(_) | HExpr::ConstB(_) | HExpr::ConstF64(_) | HExpr::VecLit(_)
        | HExpr::StrLit(_) => {}
        HExpr::StrCmp(_, a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::StrOrd(_, a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::StrNth { s, idx, .. } => {
            collect_vars(s, f);
            collect_vars(idx, f);
        }
        HExpr::CharStr(c) => collect_vars(c, f),
        HExpr::Loop { binds, body, .. } => {
            for (_, _, e2) in binds {
                collect_vars(e2, f);
            }
            collect_vars(body, f);
        }
        HExpr::Recur(args, _) => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::ToStr(e, _) => collect_vars(e, f),
        HExpr::ParseNum { s, .. } => collect_vars(s, f),
        HExpr::Slurp(p) => collect_vars(p, f),
        HExpr::Spit { path, data } => {
            collect_vars(path, f);
            collect_vars(data, f);
        }
        HExpr::Args => {}
        HExpr::Exit(c) => collect_vars(c, f),
        HExpr::StrCat(args) => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::BinW(_, a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::CmpW(_, a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::BinF(_, a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::ICast(inner, _, _) => collect_vars(inner, f),
        HExpr::MapLit(pairs) => {
            for (k, v) in pairs {
                collect_vars(k, f);
                collect_vars(v, f);
            }
        }
        HExpr::MapAssoc { m, k, v } => {
            collect_vars(m, f);
            collect_vars(k, f);
            collect_vars(v, f);
        }
        HExpr::MapGet { src, key, default, res } => {
            collect_vars(src, f);
            collect_vars(key, f);
            if let Some(d) = default {
                collect_vars(d, f);
            }
        }
        HExpr::SetLit(vs) => {
            for v in vs {
                collect_vars(v, f);
            }
        }
        HExpr::SetConj { s, x } | HExpr::SetDisj { s, x } | HExpr::SetContains { s, x } => {
            collect_vars(s, f);
            collect_vars(x, f);
        }
        HExpr::Subs { s, start, end } => {
            collect_vars(s, f);
            collect_vars(start, f);
            collect_vars(end, f);
        }
        HExpr::Split { s, sep } => {
            collect_vars(s, f);
            collect_vars(sep, f);
        }
        HExpr::Join { sep, xs } => {
            collect_vars(sep, f);
            collect_vars(xs, f);
        }
        HExpr::Closure { .. } => {
            // The lambda body is emitted out of line; captures are read
            // into the env here and are already emitted by the let.
        }
        HExpr::ClosureCall { f: callee, args, .. } => {
            collect_vars(callee, f);
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::Conj { v, x, .. } => {
            collect_vars(v, f);
            collect_vars(x, f);
        }
        HExpr::Subvec {
            v,
            start,
            end,
            ..
        } => {
            collect_vars(v, f);
            collect_vars(start, f);
            collect_vars(end, f);
        }
        HExpr::For { src, body, .. } => {
            collect_vars(src, f);
            collect_vars(body, f);
        }
        HExpr::Ctor { args, .. } => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::Match { scrut, arms, .. } => {
            collect_vars(scrut, f);
            // Arm binders are re-bound inside the helper's arm blocks, so
            // only the scrutinee and the bodies contribute captures.
            for arm in arms {
                collect_vars(&arm.body, f);
            }
        }
    }
}
