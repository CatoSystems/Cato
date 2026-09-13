use super::*;

pub(crate) fn rewrite_body(
    h: &HExpr,
    fnprefix: &str,
    params: &[(String, Ty)],
    idmap: &mut HashMap<String, usize>,
    loops: &mut Vec<LoopSpec>,
    sigs: &mut HashMap<String, (Ty, Vec<Ty>)>,
) -> Result<HExpr, String> {
    Ok(match h {
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
        HExpr::Bin(op, a, b) => HExpr::Bin(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(rewrite_body(
            e, fnprefix, params, idmap, loops, sigs,
        )?)),
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
        HExpr::Cmp(op, a, b, f) => HExpr::Cmp(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *f,
        ),
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
        HExpr::Let(binds, body, frees) => HExpr::Let(
            binds
                .iter()
                .map(|(n, e, t)| {
                    Ok((
                        n.clone(),
                        rewrite_body(e, fnprefix, params, idmap, loops, sigs)?,
                        *t,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?,
            Box::new(rewrite_body(body, fnprefix, params, idmap, loops, sigs)?),
            frees.clone(),
        ),
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
            let spec = make_loop_spec(&helper, (**r).clone(), params);
            // Base sources become data/len pairs; captures follow.
            let base_args = source_base_args(reduce_src(r));
            let mut arg_tys: Vec<Ty> = Vec::new();
            for _ in &base_args {
                arg_tys.push(VEC_I64);
            }
            for (_, t) in &spec.caps {
                arg_tys.push(*t);
            }
            sigs.insert(helper.clone(), (ret_ty, arg_tys));
            loops.push(spec);

            let mut args: Vec<HExpr> = base_args;

            let caps = &loops.last().unwrap().caps.clone();
            for (cname, _) in caps {
                args.push(HExpr::Var(cname.clone()));
            }
            HExpr::Call(helper, args)
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
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::CmpW(op, a, b, t) => HExpr::CmpW(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::BinF(op, a, b) => HExpr::BinF(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::ICast(inner, t) => HExpr::ICast(
            Box::new(rewrite_body(inner, fnprefix, params, idmap, loops, sigs)?),
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
        HExpr::MapGet { src, key, default } => HExpr::MapGet {
            src: Box::new(rewrite_body(src, fnprefix, params, idmap, loops, sigs)?),
            key: Box::new(rewrite_body(key, fnprefix, params, idmap, loops, sigs)?),
            default: match default {
                Some(d) => Some(Box::new(rewrite_body(
                    d, fnprefix, params, idmap, loops, sigs,
                )?)),
                None => None,
            },
        },
        HExpr::StrCat(args) => HExpr::StrCat(
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Conj { v, x, elem } => HExpr::Conj {
            v: Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?),
            x: Box::new(rewrite_body(x, fnprefix, params, idmap, loops, sigs)?),
            elem: *elem,
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
            body: Box::new(rewrite_body(body, fnprefix, params, idmap, loops, sigs)?),
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
                    Ok(HArm {
                        variant: arm.variant,
                        binders: arm.binders.clone(),
                        body: rewrite_body(&arm.body, fnprefix, params, idmap, loops, sigs)?,
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
