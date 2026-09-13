use super::*;

pub(crate) fn lower_body_forms(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    // items is either [LitSym("do"), form...] for multiple bodies, or we
    // receive a single list whose first element is the actual body head.
    let Some(Expr::LitSym(head)) = items.first() else {
        return Err("expected body".into());
    };
    if head == "do" {
        let mut seq = Vec::new();
        for f in &items[1..] {
            seq.push(lower(f, env, ctx)?);
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
    let tt = ty_of(&then, env, ctx);
    let et = ty_of(&els, env, ctx);
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
    // String (in)equality compares content via the runtime; ordering of
    // strings is not part of the language.
    if ty_of(&a, env, ctx) == Ty::Str && ty_of(&b, env, ctx) == Ty::Str {
        if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
            return Err("strings support only = and /=".into());
        }
        return Ok(HExpr::StrCmp(
            op == CmpOp::Eq,
            Box::new(a),
            Box::new(b),
        ));
    }
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    // Closed widths compare in their own width (and signedness); f32
    // compares with fcmp. i64 keeps the plain Cmp path.
    if at == Ty::F32 && bt == Ty::F32 {
        return Ok(HExpr::CmpW(op, Box::new(a), Box::new(b), Ty::F32));
    }
    if at == bt && at.is_int() && at != Ty::I64 {
        return Ok(HExpr::CmpW(op, Box::new(a), Box::new(b), at));
    }
    if at.is_int() && bt.is_int() && at != bt {
        return Err(format!(
            "cannot compare mixed widths {:?} and {:?} — cast explicitly",
            at, bt
        ));
    }
    if at == Ty::F32 || bt == Ty::F32 {
        return Err("cannot mix f32 with other widths — cast with (f32 e) or (f64 e)".into());
    }
    let (a, b) = promote_pair(a, b, env, ctx)?;
    let is_f64 = ty_of(&a, env, ctx) == Ty::F64;
    Ok(HExpr::Cmp(op, Box::new(a), Box::new(b), is_f64))
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
            // Map lookup: (get m k [default]) yields i64.
            let key = lower(&items[2], env, ctx)?;
            let default = match items.get(3) {
                Some(d) => Some(Box::new(lower(d, env, ctx)?)),
                None => None,
            };
            return Ok(HExpr::MapGet {
                src: Box::new(src),
                key: Box::new(key),
                default,
            });
        }
        other => return Err(format!("get source must be a vector or map, got {:?}", other)),
    };
    let idx = lower(&items[2], env, ctx)?;
    let default = match items.get(3) {
        Some(d) => Some(Box::new(lower(d, env, ctx)?)),
        None => None,
    };
    Ok(HExpr::Get {
        src: Box::new(src),
        idx: Box::new(idx),
        default,
        ty,
    })
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
        let Expr::LitSym(name) = &binds[i] else {
            return Err("let binding name must be a symbol".into());
        };
        let value = lower(&binds[i + 1], &mut child, ctx)?;
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


