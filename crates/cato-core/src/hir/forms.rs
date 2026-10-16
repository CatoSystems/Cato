use super::*;

pub(crate) fn lower_body_forms(
    items: &[Expr],
    env: &Env,
    ctx: &Ctx,
    expected: Option<Ty>,
) -> Result<HExpr, String> {
    // items is either [LitSym("do"), form...] for multiple bodies, or we
    // receive a single list whose first element is the actual body head.
    // The expected type (the defn's declared return) applies to the last
    // form — it lets nullary ctors of generic ADTs infer.
    let Some(Expr::LitSym(head)) = items.first() else {
        return Err("expected body".into());
    };
    if head == "do" {
        let mut seq = Vec::new();
        let last = items.len().saturating_sub(1);
        for (idx, f) in items[1..].iter().enumerate() {
            if idx + 1 == last {
                seq.push(lower_expect(f, env, ctx, expected)?);
            } else {
                seq.push(lower(f, env, ctx)?);
            }
        }
        Ok(if seq.len() == 1 {
            seq.pop().unwrap()
        } else {
            HExpr::Seq(seq)
        })
    } else {
        Err("internal: expected implicit do".into())
    }
}

pub(crate) fn lower_if(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 4 {
        return Err("if expects (if cond then else)".into());
    }
    let cond = lower(&items[1], env, ctx)?;
    if ty_of(&cond, env, ctx) != Ty::Bool {
        return Err("if condition must be boolean".into());
    }
    let then = lower(&items[2], env, ctx)?;
    let els = lower(&items[3], env, ctx)?;
    let mut tt = ty_of(&then, env, ctx);
    let mut et = ty_of(&els, env, ctx);
    // A `recur` branch carries no type of its own: it takes the other
    // branch's type. The branch tail may wrap the recur in control
    // structure (a match dispatch, a let, nested ifs) — any branch whose
    // every path ends in a recur never completes with a value (S8: the
    // reader's scan loop drives every continuation through recur).
    if all_paths_recur(&then) {
        tt = et;
    }
    if all_paths_recur(&els) {
        et = tt;
    }
    if tt != et && tt != Ty::F64 && et != Ty::F64 {
        return Err(format!(
            "if branches must be the same type, got {:?} and {:?}",
            tt, et
        ));
    }
    let t = if tt == Ty::F64 || et == Ty::F64 {
        Ty::F64
    } else {
        tt
    };
    Ok(HExpr::If(
        Box::new(cond),
        Box::new(coerce_f64(then, tt, t == Ty::F64)),
        Box::new(coerce_f64(els, et, t == Ty::F64)),
        t,
    ))
}

pub(crate) fn lower_cmp(op: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 {
        return Err(format!("comparison {} expects 2 operands", op));
    }
    let op = match op {
        "=" => CmpOp::Eq,
        "/=" => CmpOp::Ne,
        "<" => CmpOp::Lt,
        ">" => CmpOp::Gt,
        "<=" => CmpOp::Le,
        ">=" => CmpOp::Ge,
        _ => unreachable!(),
    };
    let a = lower(&items[1], env, ctx)?;
    let b = lower(&items[2], env, ctx)?;
    // String comparisons compare content via the runtime: = //= by
    // equality, ordering ops by the strcmp sign.
    if ty_of(&a, env, ctx) == Ty::Str && ty_of(&b, env, ctx) == Ty::Str {
        match op {
            CmpOp::Eq => {
                return Ok(HExpr::StrCmp(true, Box::new(a), Box::new(b)));
            }
            CmpOp::Ne => {
                return Ok(HExpr::StrCmp(false, Box::new(a), Box::new(b)));
            }
            CmpOp::Lt | CmpOp::Gt | CmpOp::Le | CmpOp::Ge => {
                return Ok(HExpr::StrOrd(op, Box::new(a), Box::new(b)));
            }
        }
    }
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    // Closed widths compare in their own width (and signedness); the width
    // lives in the node — no untyped compare, no silent promotion.
    if at == bt && at.is_numeric() {
        return Ok(HExpr::CmpW(op, Box::new(a), Box::new(b), at));
    }
    Err(format!(
        "cannot compare {:?} and {:?} — operands must share a width (cast explicitly)",
        at, bt
    ))
}

pub(crate) fn lower_and_or(kind: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 {
        return Err(format!("{} expects 2 operands", kind));
    }
    let a = lower(&items[1], env, ctx)?;
    let b = lower(&items[2], env, ctx)?;
    if ty_of(&a, env, ctx) != Ty::Bool || ty_of(&b, env, ctx) != Ty::Bool {
        return Err(format!("{} expects boolean operands", kind));
    }
    Ok(if kind == "and" {
        HExpr::And(Box::new(a), Box::new(b))
    } else {
        HExpr::Or(Box::new(a), Box::new(b))
    })
}

pub(crate) fn lower_not(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 2 {
        return Err("not expects 1 operand".into());
    }
    let e = lower(&items[1], env, ctx)?;
    if ty_of(&e, env, ctx) != Ty::Bool {
        return Err("not expects a boolean operand".into());
    }
    Ok(HExpr::Not(Box::new(e)))
}

pub(crate) fn lower_abs(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 2 {
        return Err("abs expects 1 operand".into());
    }
    let e = lower(&items[1], env, ctx)?;
    let t = ty_of(&e, env, ctx);
    if t != Ty::I64 && t != Ty::F64 && t != Ty::I32 {
        return Err("abs expects an i64, i32 or f64 operand".into());
    }
    Ok(HExpr::Abs(Box::new(e), t))
}

pub(crate) fn lower_min_max(kind: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 {
        return Err(format!("{} expects 2 operands", kind));
    }
    let a = lower(&items[1], env, ctx)?;
    let b = lower(&items[2], env, ctx)?;
    let (a, b) = promote_pair(a, b, env, ctx)?;
    let t = ty_of(&a, env, ctx);
    if t != Ty::I64 && t != Ty::F64 && t != Ty::I32 {
        return Err(format!("{} expects i64/i32/f64 operands", kind));
    }
    Ok(if kind == "min" {
        HExpr::Min(Box::new(a), Box::new(b), t)
    } else {
        HExpr::Max(Box::new(a), Box::new(b), t)
    })
}

pub(crate) fn lower_get(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 && items.len() != 4 {
        return Err("get expects (get <vec> <idx>) or (get <vec> <idx> <default>)".into());
    }
    let src = lower(&items[1], env, ctx)?;
    let ty = match ty_of(&src, env, ctx) {
        Ty::Vec(e) => elem_ty(e),
        Ty::Map => {
            // Map lookup: (get m k) yields (Option i64); (get m k d)
            // yields i64 directly.
            let key = lower(&items[2], env, ctx)?;
            let default = match items.get(3) {
                Some(d) => Some(Box::new(lower(d, env, ctx)?)),
                None => None,
            };
            let res = if default.is_none() {
                Some(Ty::Adt(option_instance(ctx, Ty::I64)?))
            } else {
                None
            };
            return Ok(HExpr::MapGet {
                src: Box::new(src),
                key: Box::new(key),
                default,
                res,
            });
        }
        other => return Err(format!("get source must be a vector or map, got {:?}", other)),
    };
    let idx = lower(&items[2], env, ctx)?;
    let default = match items.get(3) {
        Some(d) => Some(Box::new(lower(d, env, ctx)?)),
        None => None,
    };
    // Two-argument vector get returns (Option elem) — scalar elements only
    // (string elements would alias the vector's buffer in the Option).
    let res = if default.is_none() {
        if ty == Ty::Str {
            return Err(
                "Option get over (Vector str) is not supported — use (get v i default)".into(),
            );
        }
        Some(Ty::Adt(option_instance(ctx, ty)?))
    } else {
        None
    };
    Ok(HExpr::Get {
        src: Box::new(src),
        idx: Box::new(idx),
        default,
        ty,
        res,
    })
}

/// The monomorphized `(Option T)` instance for T, through the prelude.
pub(crate) fn option_instance(ctx: &Ctx, t: Ty) -> Result<u32, String> {
    let tpl = ctx
        .adts
        .iter()
        .position(|d| d.name == "Option" && d.ty_params.len() == 1)
        .ok_or("the prelude's (Option a) type is required for Option-returning get/nth")?;
    ctx.instantiate_adt(tpl as u32, &[t])
}

pub(crate) fn elem_ty(e: Elem) -> Ty {
    match e {
        Elem::I64 => Ty::I64,
        Elem::F64 => Ty::F64,
        Elem::Str => Ty::Str,
        Elem::Param(_) => panic!("unresolved element type parameter"),
    }
}

pub(crate) fn lower_let(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    let Expr::Vector(binds) = &items[1] else {
        return Err("let bindings must be a vector".into());
    };
    if binds.len() % 2 != 0 {
        return Err("let bindings must be key/value pairs".into());
    }
    let mut child = env.clone();
    let mut resolved = Vec::new();
    let mut i = 0;
    while i < binds.len() {
        // Vector destructuring: `[a b] expr` binds a = (get expr 0),
        // b = (get expr 1). `_` skips an element.
        if let Expr::Vector(pat) = &binds[i] {
            let value = lower(&binds[i + 1], &child, ctx)?;
            let t = ty_of(&value, &child, ctx);
            let elem = match t {
                Ty::Vec(e) => e,
                other => {
                    return Err(format!(
                        "destructured binding source must be a vector, got {:?}",
                        other
                    ))
                }
            };
            let tmp = format!("$d{}", ctx.inst.borrow().cache.len() + resolved.len());
            child.insert(tmp.clone(), Ty::Vec(elem));
            resolved.push((tmp.clone(), value, Ty::Vec(elem)));
            for (k, p) in pat.iter().enumerate() {
                let Expr::LitSym(pn) = p else {
                    return Err("destructure pattern elements must be symbols or _".into());
                };
                if pn == "_" {
                    continue;
                }
                let zero: HExpr = match elem {
                    Elem::I64 => HExpr::ConstI64(0),
                    Elem::F64 => HExpr::ConstF64(0.0),
                    other => {
                        return Err(format!(
                            "destructuring supports i64/f64 vectors, got {:?}",
                            other
                        ))
                    }
                };
                let getv = HExpr::Get {
                    src: Box::new(HExpr::Var(tmp.clone())),
                    idx: Box::new(HExpr::ConstI64(k as i64)),
                    default: Some(Box::new(zero)),
                    ty: elem_ty(elem),
                    res: None,
                };
                child.insert(pn.clone(), elem_ty(elem));
                resolved.push((pn.clone(), getv, elem_ty(elem)));
            }
            i += 2;
            continue;
        }
        let Expr::LitSym(name) = &binds[i] else {
            return Err("let binding name must be a symbol or a [a b] pattern".into());
        };
        // Optional type ascription: [x :- ty value] — pins the binding's
        // type and lets nullary ctors of generic ADTs (`(None)`) infer.
        if let Some(Expr::LitSym(sep)) = binds.get(i + 1) {
            if sep == ":-" {
                let Some(ann) = binds.get(i + 2) else {
                    return Err(format!("let binding {} expects a type after :-", name));
                };
                let Some(value_form) = binds.get(i + 3) else {
                    return Err(format!("let binding {} expects a value after the type", name));
                };
                let t = resolve_ann_ty(ann, ctx)?;
                let value = lower_expect(value_form, &child, ctx, Some(t))?;
                let vt = ty_of(&value, &child, ctx);
                let empty_vec = matches!((&t, &value), (Ty::Vec(_), HExpr::VecLit(crate::hir::VLit::I64(vs))) if vs.is_empty());
                if vt != t && !(t == Ty::F64 && vt == Ty::I64) && !empty_vec {
                    return Err(format!(
                        "let binding {} ascribed {:?} but the value is {:?}",
                        name, t, vt
                    ));
                }
                child.insert(name.clone(), t);
                resolved.push((name.clone(), value, t));
                i += 4;
                continue;
            }
        }
        let value = lower(&binds[i + 1], &child, ctx)?;
        let t = ty_of(&value, &child, ctx);
        child.insert(name.clone(), t);
        resolved.push((name.clone(), value, t));
        i += 2;
    }
    // Multiple body forms form an implicit sequence.
    let body = if items.len() > 3 {
        let mut seq = Vec::new();
        for f in &items[2..] {
            seq.push(lower(f, &child, ctx)?);
        }
        HExpr::Seq(seq)
    } else {
        lower(&items[2], &child, ctx)?
    };
    Ok(HExpr::Let(resolved, Box::new(body), Vec::new()))
}



/// Does every path through `e` end in a `recur`? Such an expression never
/// completes with a value, so an `if` with it on one side takes the other
/// side's type. Reads the same tail positions `validate_recurs` accepts.
pub(crate) fn all_paths_recur(e: &HExpr) -> bool {
    match e {
        HExpr::Recur(_) => true,
        HExpr::If(_, t, f, _) => all_paths_recur(t) && all_paths_recur(f),
        HExpr::Match { arms, .. } => arms.iter().all(|a| all_paths_recur(&a.body)),
        HExpr::Let(_, body, _) => all_paths_recur(body),
        HExpr::Seq(vs) => vs.last().map(all_paths_recur).unwrap_or(false),
        _ => false,
    }
}
