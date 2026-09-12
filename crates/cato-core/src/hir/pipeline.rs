use super::*;

pub(crate) fn coerce_f64(e: HExpr, cur: Ty, want: bool) -> HExpr {
    if want && cur != Ty::F64 {
        HExpr::ConvF64(Box::new(e))
    } else {
        e
    }
}

pub(crate) fn promote_pair(a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> Result<(HExpr, HExpr), String> {
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    if matches!(at, Ty::Adt(_) | Ty::Str) || matches!(bt, Ty::Adt(_) | Ty::Str) {
        return Err("ADT and string values cannot be used in arithmetic".into());
    }
    // i32 is a closed width: both operands must be i32 (cast explicitly).
    if at == Ty::I32 && bt == Ty::I32 {
        return Ok((a, b));
    }
    if at == Ty::I32 || bt == Ty::I32 {
        return Err(
            "cannot mix i32 with other widths — cast with (i64 e) or (i32 e)".into(),
        );
    }
    let f = at == Ty::F64 || bt == Ty::F64;
    Ok((coerce_f64(a, at, f), coerce_f64(b, bt, f)))
}

pub(crate) fn promote_bin(op: BinOp, a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    let (a, b) = promote_pair(a, b, env, ctx)?;
    if ty_of(&a, env, ctx) == Ty::I32 {
        return Ok(HExpr::BinW(op, Box::new(a), Box::new(b), Ty::I32));
    }
    Ok(HExpr::Bin(op, Box::new(a), Box::new(b)))
}

pub(crate) fn promote_init(init: HExpr) -> HExpr {
    match init {
        HExpr::ConstI64(v) => HExpr::ConstF64(v as f64),
        other => HExpr::ConvF64(Box::new(other)),
    }
}

pub(crate) fn lower_reduce(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() < 4 {
        return Err(format!(
            "reduce expects (reduce <op|fn> <init> <src>), got {} arguments",
            items.len().saturating_sub(1)
        ));
    }
    let head = &items[1];
    match head {
        Expr::LitSym(op) if matches!(op.as_str(), "+" | "-" | "*" | "/") => {
            let combine = match op.as_str() {
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                _ => BinOp::Div,
            };
            let init = lower(&items[2], env, ctx)?;
            let (src, maps, filters) = pipeline_src(&items[3], env, ctx)?;
            // f64 stage results force an f64 accumulator (mirrors `if`).
            let (src_el, _) = source_elem_tys(&src, env)?;
            let stage_ty = maps.last().map(|m| m.res_ty).unwrap_or(src_el);
            let init = if stage_ty == Ty::F64 && ty_of(&init, env, ctx) == Ty::I64 {
                promote_init(init)
            } else {
                init
            };
            Ok(HExpr::Reduce(Box::new(HReduce::Pipeline {
                combine,
                init,
                maps,
                filters,
                src,
            })))
        }
        _ => {
            let Expr::List(fnitems) = head else {
                return Err("reduce expects a combine op or (fn ...)".into());
            };
            if fnitems.len() < 3 {
                return Err("reduce fn expects (fn [acc el] <body>)".into());
            }
            let Expr::Vector(params) = &fnitems[1] else {
                return Err("reduce fn params".into());
            };
            let mut init = lower(&items[2], env, ctx)?;
            let mut init_ty = ty_of(&init, env, ctx);
            let (src, maps, filters) = pipeline_src(&items[3], env, ctx)?;
            let zip = matches!(src, HSource::Zip(..));
            let arity = if zip { 3 } else { 2 };
            if params.len() != arity {
                return Err(format!(
                    "reduce fn must bind exactly [acc el{}] here",
                    if zip { " el2" } else { "" }
                ));
            }
            let acc = param_name(&params[0])?;
            let el = param_name(&params[1])?;
            let el2 = if zip {
                Some(param_name(&params[2])?)
            } else {
                None
            };
            let (src_el, src_el2) = source_elem_tys(&src, env)?;
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(src_el);
            let el2_ty = maps.last().map(|_| Ty::I64).unwrap_or(src_el2);
            // f64 elements force an f64 accumulator (mirrors `if`); int
            // inits promote and int bodies coerce.
            if el_ty == Ty::F64 && init_ty == Ty::I64 {
                init_ty = Ty::F64;
                init = promote_init(init);
            }
            let mut child = env.clone();
            child.insert(acc.clone(), init_ty);
            child.insert(el.clone(), el_ty);
            if let Some(e2) = &el2 {
                child.insert(e2.clone(), el2_ty);
            }
            let mut body = lower(&fnitems[2], &child, ctx)?;
            if init_ty == Ty::F64 {
                let body_ty = ty_of(&body, &child, ctx);
                if body_ty == Ty::I64 {
                    body = coerce_f64(body, body_ty, true);
                }
            }
            Ok(HExpr::Reduce(Box::new(HReduce::Closure {
                init,
                src,
                acc,
                el,
                el2,
                body,
                maps,
                filters,
            })))
        }
    }
}

pub(crate) fn pipeline_src(
    src: &Expr,
    env: &Env,
    ctx: &Ctx,
) -> Result<(HSource, Vec<MapSpec>, Vec<FilterSpec>), String> {
    let Expr::List(items) = src else {
        return Ok((expr_to_source(src, env, ctx)?, Vec::new(), Vec::new()));
    };
    let Some(Expr::LitSym(head)) = items.first() else {
        return Ok((expr_to_source(src, env, ctx)?, Vec::new(), Vec::new()));
    };
    match head.as_str() {
        "map" => {
            if items.len() < 3 {
                return Err("map expects (map <fn> <src>)".into());
            }
            let (inner, mut maps, filters) = pipeline_src(&items[2], env, ctx)?;
            let (src_el, src_el2) = source_elem_tys(&inner, env)?;
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(src_el);
            let el2_ty = maps.last().map(|_| Ty::I64).unwrap_or(src_el2);
            let zip = matches!(inner, HSource::Zip(..));
            let (el, el2, body, res_ty) = lambda_spec(&items[1], zip, el_ty, el2_ty, env, ctx)?;
            maps.push(MapSpec {
                el,
                el2,
                el2_ty,
                body,
                el_ty,
                res_ty,
            });
            Ok((inner, maps, filters))
        }
        "filter" => {
            if items.len() < 3 {
                return Err("filter expects (filter <fn> <src>)".into());
            }
            let (inner, maps, mut filters) = pipeline_src(&items[2], env, ctx)?;
            let (src_el, src_el2) = source_elem_tys(&inner, env)?;
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(src_el);
            let el2_ty = maps.last().map(|_| Ty::I64).unwrap_or(src_el2);
            let zip = matches!(inner, HSource::Zip(..));
            let (el, el2, body, _) = lambda_spec(&items[1], zip, el_ty, el2_ty, env, ctx)?;
            filters.push(FilterSpec {
                el,
                el2,
                el2_ty,
                body,
                el_ty,
            });
            Ok((inner, maps, filters))
        }
        "take" => {
            if items.len() != 3 {
                return Err("take expects (take <n> <src>)".into());
            }
            let n = lower(&items[1], env, ctx)?;
            let (inner, maps, filters) = pipeline_src(&items[2], env, ctx)?;
            Ok((HSource::Take(Box::new(n), Box::new(inner)), maps, filters))
        }
        "drop" => {
            if items.len() != 3 {
                return Err("drop expects (drop <n> <src>)".into());
            }
            let n = lower(&items[1], env, ctx)?;
            let (inner, maps, filters) = pipeline_src(&items[2], env, ctx)?;
            Ok((HSource::Drop(Box::new(n), Box::new(inner)), maps, filters))
        }
        "zip" => {
            if items.len() != 3 {
                return Err("zip expects (zip <xs> <ys>)".into());
            }
            let a = vec_source_arg(&items[1], env, ctx)?;
            let b = vec_source_arg(&items[2], env, ctx)?;
            Ok((HSource::Zip(Box::new(a), Box::new(b)), Vec::new(), Vec::new()))
        }
        "range" => {
            match items.len() {
                2 => {
                    let end = lower(&items[1], env, ctx)?;
                    Ok((
                        HSource::Range(Box::new(HExpr::ConstI64(0)), Box::new(end)),
                        Vec::new(),
                        Vec::new(),
                    ))
                }
                3 => {
                    let a = lower(&items[1], env, ctx)?;
                    let b = lower(&items[2], env, ctx)?;
                    Ok((HSource::Range(Box::new(a), Box::new(b)), Vec::new(), Vec::new()))
                }
                n => Err(format!("range expects 1 or 2 arguments, got {}", n - 1)),
            }
        }
        _ => Ok((expr_to_source(src, env, ctx)?, Vec::new(), Vec::new())),
    }
}

/// A plain vector source: a variable bound to a vector or an inline
/// vector literal.
pub(crate) fn expr_to_source(src: &Expr, env: &Env, ctx: &Ctx) -> Result<HSource, String> {
    match src {
        Expr::LitSym(v) => Ok(HSource::Var(v.clone())),
        Expr::Vector(items) if items.iter().all(|e| matches!(e, Expr::LitI64(_))) => {
            Ok(HSource::Lit(VLit::I64(vec_lit_i64(items))))
        }
        Expr::List(items)
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "vec-i64")
                && items[1..].iter().all(|e| matches!(e, Expr::LitI64(_))) =>
        {
            Ok(HSource::Lit(VLit::I64(vec_lit_i64(&items[1..]))))
        }
        Expr::List(items)
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "vec-f64")
                && items[1..].iter().all(|e| matches!(e, Expr::LitI64(_) | Expr::LitF64(_))) =>
        {
            Ok(HSource::Lit(VLit::F64(vec_lit_f64(&items[1..]))))
        }
        _ => {
            let _ = lower(src, env, ctx);
            Err("reduce source must be a vector variable or literal".into())
        }
    }
}

/// Element type(s) a loop source yields: (first, second) — the second is
/// `Ty::I64` (unused) for non-zip sources.
pub(crate) fn source_elem_tys(src: &HSource, env: &Env) -> Result<(Ty, Ty), String> {
    match src {
        HSource::Var(v) => match env.get(v) {
            Some(Ty::Vec(Elem::I64)) => Ok((Ty::I64, Ty::I64)),
            Some(Ty::Vec(Elem::F64)) => Ok((Ty::F64, Ty::F64)),
            Some(other) => Err(format!("loop source {} is not a vector ({:?})", v, other)),
            None => Err(format!("unknown loop source {}", v)),
        },
        HSource::Lit(vl) => Ok(match vl {
            VLit::I64(_) => (Ty::I64, Ty::I64),
            VLit::F64(_) => (Ty::F64, Ty::F64),
        }),
        HSource::Range(..) => Ok((Ty::I64, Ty::I64)),
        HSource::Zip(a, b) => Ok((source_elem_tys(a, env)?.0, source_elem_tys(b, env)?.0)),
        HSource::Take(_, inner) | HSource::Drop(_, inner) => source_elem_tys(inner, env),
    }
}

pub(crate) fn vec_source_arg(src: &Expr, _env: &Env, _ctx: &Ctx) -> Result<HSource, String> {
    match src {
        Expr::LitSym(v) => Ok(HSource::Var(v.clone())),
        _ => Err("zip arguments must be vector variables".into()),
    }
}

