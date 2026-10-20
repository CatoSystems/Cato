use super::*;

/// The typed bindings visible at a node (parameters, then every `let`,
/// `loop`, `for`, match binder in scope — innermost last) plus the ADT
/// table for binder field types. A hoisted reduce helper takes its
/// captures' and base vectors' types from here: before, only the defn's
/// parameters were known and everything else was *guessed* (a `let`-bound
/// i64 capture became a `double` parameter — invalid IR).
pub(crate) struct Scope<'a> {
    pub(crate) vars: Vec<(String, Ty)>,
    pub(crate) adts: &'a [AdtDef],
}

impl<'a> Scope<'a> {
    pub(crate) fn new(vars: Vec<(String, Ty)>, adts: &'a [AdtDef]) -> Self {
        Scope { vars, adts }
    }
    fn with(&self, more: impl IntoIterator<Item = (String, Ty)>) -> Scope<'a> {
        let mut vars = self.vars.clone();
        vars.extend(more);
        Scope { vars, adts: self.adts }
    }
    fn ty_of_var(&self, v: &str) -> Option<Ty> {
        self.vars.iter().rev().find(|(n, _)| n == v).map(|(_, t)| *t)
    }
    /// Field types of a match arm's binders, when the scrutinee's ADT is
    /// known (a typed variable, or a call through the signature table).
    fn binder_tys(
        &self,
        scrut: &HExpr,
        arm: &HArm,
        sigs: &HashMap<String, (Ty, Vec<Ty>)>,
    ) -> Vec<(String, Ty)> {
        let adt = match scrut {
            HExpr::Var(v) => self.ty_of_var(v),
            HExpr::Call(f, _) => sigs.get(f).map(|(r, _)| *r),
            HExpr::Ctor { adt, .. } => Some(Ty::Adt(*adt)),
            _ => None,
        };
        let (Some(Ty::Adt(i)), Some(vi)) = (adt, arm.variant) else {
            return Vec::new();
        };
        let Some(fields) = self.adts.get(i as usize).and_then(|d| d.variants.get(vi as usize)) else {
            return Vec::new();
        };
        arm.binders.iter().cloned().zip(fields.1.iter().copied()).collect()
    }
}

pub(crate) fn rewrite_body(
    h: &HExpr,
    fnprefix: &str,
    params: &Scope,
    idmap: &mut HashMap<String, usize>,
    loops: &mut Vec<LoopSpec>,
    sigs: &mut HashMap<String, (Ty, Vec<Ty>)>,
) -> Result<HExpr, String> {
    Ok(match h {
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstB(v) => HExpr::ConstB(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::StrOrd(op, a, b) => HExpr::StrOrd(
            *op,
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::StrNth { s, idx, res, .. } => HExpr::StrNth {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            idx: Box::new(rewrite_body(idx, fnprefix, params, idmap, loops, sigs)?),
            res: *res,
        },
        HExpr::CharStr(c) => HExpr::CharStr(Box::new(rewrite_body(
            c,
            fnprefix,
            params,
            idmap,
            loops,
            sigs,
        )?)),
        HExpr::Loop { binds, body, ty } => HExpr::Loop {
            binds: binds
                .iter()
                .map(|(n, t, e2)| {
                    Ok((
                        n.clone(),
                        *t,
                        rewrite_body(e2, fnprefix, params, idmap, loops, sigs)?,
                    ))
                })
                .collect::<Result<Vec<(String, Ty, HExpr)>, String>>()?,
            body: Box::new(rewrite_body(
                body,
                fnprefix,
                &params.with(binds.iter().map(|(n, t, _)| (n.clone(), *t))),
                idmap,
                loops,
                sigs,
            )?),
            ty: *ty,
        },
        HExpr::Recur(args, drops) => HExpr::Recur(
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<HExpr>, String>>()?,
            drops.clone(),
        ),
        HExpr::ToStr(e2, t) => HExpr::ToStr(
            Box::new(rewrite_body(e2, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::ParseNum { s, int, res } => HExpr::ParseNum {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            int: *int,
            res: *res,
        },
        HExpr::Slurp(p) => HExpr::Slurp(Box::new(rewrite_body(
            p,
            fnprefix,
            params,
            idmap,
            loops,
            sigs,
        )?)),
        HExpr::Spit { path, data } => HExpr::Spit {
            path: Box::new(rewrite_body(path, fnprefix, params, idmap, loops, sigs)?),
            data: Box::new(rewrite_body(data, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::Args => HExpr::Args,
        HExpr::Exit(c) => HExpr::Exit(Box::new(rewrite_body(
            c,
            fnprefix,
            params,
            idmap,
            loops,
            sigs,
        )?)),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
        HExpr::Count(e) => {
            HExpr::Count(Box::new(rewrite_body(e, fnprefix, params, idmap, loops, sigs)?))
        }
        HExpr::If(c, t, e, ty) => HExpr::If(
            Box::new(rewrite_body(c, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(t, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(e, fnprefix, params, idmap, loops, sigs)?),
            *ty,
        ),
        HExpr::And(a, b) => HExpr::And(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Or(a, b) => HExpr::Or(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Not(v) => HExpr::Not(Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?)),
        HExpr::Abs(v, t) => {
            HExpr::Abs(Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?), *t)
        }
        HExpr::Min(a, b, t) => HExpr::Min(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::Max(a, b, t) => HExpr::Max(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::Call(name, args) => HExpr::Call(
            name.clone(),
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Let(binds, body, frees) => {
            // Sequential: each value sees the bindings before it.
            let mut scope = params.with(std::iter::empty());
            let mut out = Vec::with_capacity(binds.len());
            for (n, e, t) in binds {
                out.push((n.clone(), rewrite_body(e, fnprefix, &scope, idmap, loops, sigs)?, *t));
                scope.vars.push((n.clone(), *t));
            }
            HExpr::Let(
                out,
                Box::new(rewrite_body(body, fnprefix, &scope, idmap, loops, sigs)?),
                frees.clone(),
            )
        }
        HExpr::Seq(vs) => HExpr::Seq(
            vs.iter()
                .map(|v| rewrite_body(v, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Reduce(r) => {
            let id = idmap.entry(fnprefix.to_string()).or_insert(0);
            let helper = format!("{}_reduce_{}", fnprefix, id);
            *id += 1;

            let ret_f64 = acc_ret_is_f64(r);
            let ret_ty = if ret_f64 { Ty::F64 } else { Ty::I64 };
            let spec = make_loop_spec(&helper, (**r).clone(), &params.vars);
            // Base sources become data/len pairs; captures follow.
            let base_args = source_base_args(reduce_src(r));
            let mut arg_tys: Vec<Ty> = Vec::new();
            for _ in &base_args {
                arg_tys.push(VEC_I64);
            }
            for (_, t) in &spec.caps {
                arg_tys.push(*t);
            }
            sigs.insert(helper.clone(), (ret_ty, arg_tys.clone()));
            let par_name = if spec.parallel {
                sigs.insert(format!("{}_par", helper), (ret_ty, arg_tys.clone()));
                format!("{}_par", helper)
            } else {
                helper.clone()
            };
            loops.push(spec);

            let mut args: Vec<HExpr> = base_args;

            let caps = &loops.last().unwrap().caps.clone();
            for (cname, _) in caps {
                args.push(HExpr::Var(cname.clone()));
            }
            HExpr::Call(par_name, args)
        }
        HExpr::Ctor { adt, variant, args } => HExpr::Ctor {
            adt: *adt,
            variant: *variant,
            args: args
                .iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        },
        HExpr::StrLit(s) => HExpr::StrLit(s.clone()),
        HExpr::BinW(op, a, b, t) => HExpr::BinW(
            *op,
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::CmpW(op, a, b, t) => HExpr::CmpW(
            *op,
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::BinF(op, a, b) => HExpr::BinF(
            *op,
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::ICast(inner, from, t) => HExpr::ICast(
            Box::new(rewrite_body(inner, fnprefix, params, idmap, loops, sigs)?),
            *from,
            *t,
        ),
        HExpr::MapLit(pairs) => HExpr::MapLit(
            pairs
                .iter()
                .map(|(k, v)| {
                    Ok((
                        rewrite_body(k, fnprefix, params, idmap, loops, sigs)?,
                        rewrite_body(v, fnprefix, params, idmap, loops, sigs)?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?,
        ),
        HExpr::MapAssoc { m, k, v } => HExpr::MapAssoc {
            m: Box::new(rewrite_body(m, fnprefix, params, idmap, loops, sigs)?),
            k: Box::new(rewrite_body(k, fnprefix, params, idmap, loops, sigs)?),
            v: Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::MapGet { src, key, default, res } => HExpr::MapGet {
            src: Box::new(rewrite_body(src, fnprefix, params, idmap, loops, sigs)?),
            key: Box::new(rewrite_body(key, fnprefix, params, idmap, loops, sigs)?),
            default: match default {
                Some(d) => Some(Box::new(rewrite_body(
                    d, fnprefix, params, idmap, loops, sigs,
                )?)),
                None => None,
            },
            res: *res,
        },
        HExpr::StrCat(args) => HExpr::StrCat(
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Conj { v, x, elem, in_place } => HExpr::Conj {
            v: Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?),
            x: Box::new(rewrite_body(x, fnprefix, params, idmap, loops, sigs)?),
            elem: *elem,
            in_place: *in_place,
        },
        HExpr::Subvec {
            v,
            start,
            end,
            elem,
        } => HExpr::Subvec {
            v: Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?),
            start: Box::new(rewrite_body(start, fnprefix, params, idmap, loops, sigs)?),
            end: Box::new(rewrite_body(end, fnprefix, params, idmap, loops, sigs)?),
            elem: *elem,
        },
        HExpr::For {
            el,
            src,
            elem,
            body,
            res_elem,
        } => HExpr::For {
            el: el.clone(),
            src: Box::new(rewrite_body(src, fnprefix, params, idmap, loops, sigs)?),
            elem: *elem,
            body: Box::new(rewrite_body(
                body,
                fnprefix,
                &params.with(elem.ty().map(|t| (el.clone(), t))),
                idmap,
                loops,
                sigs,
            )?),
            res_elem: *res_elem,
        },
        HExpr::StrCmp(p, a, b) => HExpr::StrCmp(
            *p,
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Clone(inner) => {
            HExpr::Clone(Box::new(rewrite_body(inner, fnprefix, params, idmap, loops, sigs)?))
        }
        HExpr::DropStmt(inner) => HExpr::DropStmt(Box::new(rewrite_body(
            inner, fnprefix, params, idmap, loops, sigs,
        )?)),
        HExpr::Get {
            src,
            idx,
            default,
            ty,
            res,
        } => HExpr::Get {
            src: Box::new(rewrite_body(src, fnprefix, params, idmap, loops, sigs)?),
            idx: Box::new(rewrite_body(idx, fnprefix, params, idmap, loops, sigs)?),
            default: match default {
                Some(d) => Some(Box::new(rewrite_body(
                    d, fnprefix, params, idmap, loops, sigs,
                )?)),
                None => None,
            },
            ty: *ty,
            res: *res,
        },
        HExpr::Closure { sig, mangled, params, captures, capture_owns, body } => HExpr::Closure {
            sig: *sig,
            mangled: mangled.clone(),
            params: params.clone(),
            captures: captures.clone(),
            capture_owns: capture_owns.clone(),
            // The out-of-line lambda body is rewritten separately; the
            // in-node copy stays intact for the interpreter.
            body: Box::new((**body).clone()),
        },
        HExpr::ClosureCall { f, args, sig } => HExpr::ClosureCall {
            f: Box::new(rewrite_body(f, fnprefix, params, idmap, loops, sigs)?),
            args: args
                .iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
            sig: *sig,
        },
        HExpr::SetLit(vs) => HExpr::SetLit(
            vs.iter()
                .map(|v| rewrite_body(v, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::SetConj { s, x } => HExpr::SetConj {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            x: Box::new(rewrite_body(x, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::SetDisj { s, x } => HExpr::SetDisj {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            x: Box::new(rewrite_body(x, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::SetContains { s, x } => HExpr::SetContains {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            x: Box::new(rewrite_body(x, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::Subs { s, start, end } => HExpr::Subs {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            start: Box::new(rewrite_body(start, fnprefix, params, idmap, loops, sigs)?),
            end: Box::new(rewrite_body(end, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::Split { s, sep } => HExpr::Split {
            s: Box::new(rewrite_body(s, fnprefix, params, idmap, loops, sigs)?),
            sep: Box::new(rewrite_body(sep, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::Join { sep, xs } => HExpr::Join {
            sep: Box::new(rewrite_body(sep, fnprefix, params, idmap, loops, sigs)?),
            xs: Box::new(rewrite_body(xs, fnprefix, params, idmap, loops, sigs)?),
        },
        HExpr::Match { scrut, arms, ty } => HExpr::Match {
            scrut: Box::new(rewrite_body(scrut, fnprefix, params, idmap, loops, sigs)?),
            arms: arms
                .iter()
                .map(|arm| {
                    let scope = params.with(params.binder_tys(scrut, arm, sigs));
                    Ok(HArm {
                        variant: arm.variant,
                        binders: arm.binders.clone(),
                        body: rewrite_body(&arm.body, fnprefix, &scope, idmap, loops, sigs)?,
                        exit_free: arm.exit_free.clone(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            ty: *ty,
        },
    })
}

// ---------------------------------------------------------------------------
// Module buffer
// ---------------------------------------------------------------------------

/// A user fn's returned `str` is owned by the caller (the owning-temp
/// convention: str results are caller-freed), but a borrowed-shape return —
/// a literal, a vector element load, or a control composite whose leaves are
/// borrowed — hands the caller a pointer it will free at scope exit while
/// the vector (or the static literal pool) still owns it. Wrap every
/// borrowed leaf in `clone` (the strdup materialization) at the return
/// boundary. Fresh temps (subs/strcat/...) pass through, and plain Vars pass
/// through: an owning temp transfers its drop duty via the tail-escape rule,
/// and a non-owning alias's duty stays with the caller's own binding, which
/// the ownership planner already accounted for.
pub(crate) fn own_str_return(e: &HExpr) -> HExpr {
    match e {
        HExpr::StrLit(_) | HExpr::Get { ty: Ty::Str, res: None, .. } => {
            HExpr::Clone(Box::new(e.clone()))
        }
        HExpr::Seq(vs) => HExpr::Seq(
            vs.iter()
                .enumerate()
                .map(|(i, v)| {
                    if i + 1 == vs.len() {
                        own_str_return(v)
                    } else {
                        v.clone()
                    }
                })
                .collect(),
        ),
        HExpr::If(cond, t, f, ty) => HExpr::If(
            cond.clone(),
            Box::new(own_str_return(t)),
            Box::new(own_str_return(f)),
            *ty,
        ),
        HExpr::Let(binds, body, frees) => {
            HExpr::Let(binds.clone(), Box::new(own_str_return(body)), frees.clone())
        }
        HExpr::Match { scrut, arms, ty } => HExpr::Match {
            scrut: scrut.clone(),
            arms: arms
                .iter()
                .map(|arm| HArm {
                    variant: arm.variant,
                    binders: arm.binders.clone(),
                    body: own_str_return(&arm.body),
                    exit_free: arm.exit_free.clone(),
                })
                .collect(),
            ty: *ty,
        },
        _ => e.clone(),
    }
}
