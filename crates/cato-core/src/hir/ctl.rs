use super::*;

pub(crate) fn has_ctl(e: &HExpr) -> bool {
    match e {
        HExpr::If(..) | HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Match { .. } => {
            true
        }
        HExpr::Count(x) | HExpr::Abs(x, _) => has_ctl(x),
        HExpr::Call(_, args) => args.iter().any(has_ctl),
        HExpr::Ctor { args, .. } => args.iter().any(has_ctl),
        HExpr::Get { idx, default, .. } => {
            has_ctl(idx) || default.as_deref().map(has_ctl).unwrap_or(false)
        }
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::StrCmp(..)
        | HExpr::StrOrd(..)
        | HExpr::Var(_)
        | HExpr::ConstI64(_)
        | HExpr::ConstB(_)
        | HExpr::ConstF64(_)
        | HExpr::VecLit(_)
        | HExpr::StrLit(_) => false,
        HExpr::StrNth { s, idx, .. } => has_ctl(s) || has_ctl(idx),
        HExpr::CharStr(c) => has_ctl(c),
        HExpr::Loop { .. } | HExpr::Recur(..) => true,
        HExpr::ToStr(e, _) => has_ctl(e),
        HExpr::ParseNum { s, .. } => has_ctl(s),
        HExpr::Slurp(p) => has_ctl(p),
        HExpr::Spit { path, data } => has_ctl(path) || has_ctl(data),
        HExpr::Args => false,
        HExpr::Exit(c) => has_ctl(c),
        HExpr::Conj { .. } => true,
        HExpr::Subvec { v, start, end, .. } => has_ctl(v) || has_ctl(start) || has_ctl(end),
        HExpr::For { .. } => true,
        HExpr::StrCat(args) => args.iter().any(has_ctl),
        HExpr::BinW(_, a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::CmpW(_, a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::BinF(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::ICast(inner, _, _) => has_ctl(inner),
        HExpr::MapLit(pairs) => pairs.iter().any(|(k, v)| has_ctl(k) || has_ctl(v)),
        HExpr::MapAssoc { m, k, v } => has_ctl(m) || has_ctl(k) || has_ctl(v),
        HExpr::MapGet {
            src, key, default, ..
        } => has_ctl(src) || has_ctl(key) || default.as_deref().map(has_ctl).unwrap_or(false),
        HExpr::SetLit(vs) => vs.iter().any(has_ctl),
        HExpr::SetConj { s, x } | HExpr::SetDisj { s, x } | HExpr::SetContains { s, x } => {
            has_ctl(s) || has_ctl(x)
        }
        HExpr::Subs { s, start, end } => has_ctl(s) || has_ctl(start) || has_ctl(end),
        HExpr::Split { s, sep } => has_ctl(s) || has_ctl(sep),
        HExpr::Join { sep, xs } => has_ctl(sep) || has_ctl(xs),
        HExpr::Clone(x) | HExpr::DropStmt(x) => has_ctl(x),
        HExpr::Closure { body, .. } => has_ctl(body),
        HExpr::ClosureCall { f, args, .. } => has_ctl(f) || args.iter().any(has_ctl),
        HExpr::Let(binds, body, _) => binds.iter().any(|(_, e, _)| has_ctl(e)) || has_ctl(body),
        HExpr::Seq(vs) => vs.iter().any(has_ctl),
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline {
                init,
                maps,
                filters,
                src,
                ..
            } => {
                has_ctl(init)
                    || has_ctl_source(src)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
            HReduce::Closure {
                init,
                src,
                body,
                maps,
                filters,
                ..
            } => {
                has_ctl(init)
                    || has_ctl_source(src)
                    || has_ctl(body)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
        },
    }
}

pub(crate) fn has_ctl_source(src: &HSource) -> bool {
    match src {
        HSource::Var(_) | HSource::Lit(_) => false,
        HSource::Range(a, b) => has_ctl(a) || has_ctl(b),
        HSource::Zip(a, b) => has_ctl_source(a) || has_ctl_source(b),
        HSource::Take(n, inner) | HSource::Drop(n, inner) => has_ctl(n) || has_ctl_source(inner),
    }
}
