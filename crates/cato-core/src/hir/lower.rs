use super::*;

pub(crate) fn lower(expr: &Expr, env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    match expr {
        Expr::LitI64(v) => Ok(HExpr::ConstI64(*v)),
        Expr::LitF64(v) => Ok(HExpr::ConstF64(*v)),
        Expr::LitStr(s) => Ok(HExpr::StrLit(s.clone())),
        Expr::LitSym(s) => Ok(HExpr::Var(s.to_string())),
        Expr::MapLit(pairs) => {
            let mut out = Vec::new();
            for (k, v) in pairs {
                let ke = lower(k, env, ctx)?;
                if ty_of(&ke, env, ctx) != Ty::I64 {
                    return Err("map keys must be i64".into());
                }
                let ve = lower(v, env, ctx)?;
                if ty_of(&ve, env, ctx) != Ty::I64 {
                    return Err("map values must be i64".into());
                }
                out.push((ke, ve));
            }
            // Sort by key at compile time; duplicate keys: last wins.
            out.sort_by_key(|(k, _)| match k {
                HExpr::ConstI64(c) => *c,
                _ => i64::MAX,
            });
            Ok(HExpr::MapLit(out))
        }
        Expr::SetLit(items) => {
            // Constant elements only, like map literals; sorted + deduped.
            let mut vs = Vec::new();
            for it in items {
                match it {
                    Expr::LitI64(v) => vs.push(*v),
                    other => {
                        return Err(format!(
                            "set literal elements must be i64 literals, got {:?}",
                            other
                        ))
                    }
                }
            }
            vs.sort_unstable();
            vs.dedup();
            Ok(HExpr::SetLit(vs.into_iter().map(HExpr::ConstI64).collect()))
        }
        Expr::Vector(items) => {
            if items.iter().all(|e| matches!(e, Expr::LitI64(_))) {
                Ok(HExpr::VecLit(VLit::I64(vec_lit_i64(items))))
            } else if items.iter().all(|e| matches!(e, Expr::LitI64(_) | Expr::LitF64(_))) {
                Ok(HExpr::VecLit(VLit::F64(vec_lit_f64(items))))
            } else {
                Err("vector literal not supported (use (vec-i64 ...) / (vec-f64 ...))".into())
            }
        }
        Expr::List(items) => {
            let Some(Expr::LitSym(head)) = items.first() else {
                return Err("form head must be a symbol".into());
            };
            match head.as_str() {
                "do" => {
                    let mut seq = Vec::new();
                    for f in &items[1..] {
                        seq.push(lower(f, env, ctx)?);
                    }
                    Ok(if seq.len() == 1 {
                        seq.pop().unwrap()
                    } else {
                        HExpr::Seq(seq)
                    })
                }
                "fn" => Err("lambda must be consumed by reduce/map/filter".into()),
                "let" => lower_let(items, env, ctx),
                "reduce" => lower_reduce(items, env, ctx),
                "map" | "filter" => Err(format!(
                    "{} must appear as the source of a reduce in the slice",
                    head
                )),
                "f64" => {
                    if items.len() != 2 {
                        return Err("f64 expects exactly 1 argument".into());
                    }
                    let inner = lower(&items[1], env, ctx)?;
                    Ok(HExpr::ConvF64(Box::new(inner)))
                }
                "i32" | "i64" => {
                    if items.len() != 2 {
                        return Err(format!("{} expects exactly 1 argument", head));
                    }
                    let inner = lower(&items[1], env, ctx)?;
                    let target = if head == "i32" { Ty::I32 } else { Ty::I64 };
                    let src_ty = ty_of(&inner, env, ctx);
                    if src_ty == target {
                        return Ok(inner);
                    }
                    if !matches!(src_ty, Ty::I64 | Ty::I32) {
                        return Err(format!(
                            "{} cast expects an i64/i32 operand, got {:?}",
                            head, src_ty
                        ));
                    }
                    Ok(HExpr::ICast(Box::new(inner), target))
                }
                "if" => lower_if(items, env, ctx),
                "and" | "or" => lower_and_or(head, items, env, ctx),
                "not" => lower_not(items, env, ctx),
                "=" | "/=" | "<" | ">" | "<=" | ">=" => lower_cmp(head, items, env, ctx),
                "abs" => lower_abs(items, env, ctx),
                "min" | "max" => lower_min_max(head, items, env, ctx),
                "match" => lower_match(items, env, ctx),
                "get" => lower_get(items, env, ctx),
                "contains?" => {
                    if items.len() != 3 {
                        return Err("contains? expects (contains? <set> <x>)".into());
                    }
                    let s = lower(&items[1], env, ctx)?;
                    if ty_of(&s, env, ctx) != Ty::Set {
                        return Err("contains? source must be a set".into());
                    }
                    let x = lower(&items[2], env, ctx)?;
                    if ty_of(&x, env, ctx) != Ty::I64 {
                        return Err("contains? membership key must be i64".into());
                    }
                    Ok(HExpr::SetContains {
                        s: Box::new(s),
                        x: Box::new(x),
                    })
                }
                "disj" => {
                    if items.len() != 3 {
                        return Err("disj expects (disj <set> <x>)".into());
                    }
                    let s = lower(&items[1], env, ctx)?;
                    if ty_of(&s, env, ctx) != Ty::Set {
                        return Err("disj source must be a set".into());
                    }
                    let x = lower(&items[2], env, ctx)?;
                    if ty_of(&x, env, ctx) != Ty::I64 {
                        return Err("disj element must be i64".into());
                    }
                    Ok(HExpr::SetDisj {
                        s: Box::new(s),
                        x: Box::new(x),
                    })
                }
                "subs" => {
                    if items.len() != 4 {
                        return Err("subs expects (subs <s> <start> <end>)".into());
                    }
                    let s = lower(&items[1], env, ctx)?;
                    if ty_of(&s, env, ctx) != Ty::Str {
                        return Err("subs source must be a string".into());
                    }
                    let start = lower(&items[2], env, ctx)?;
                    let end = lower(&items[3], env, ctx)?;
                    Ok(HExpr::Subs {
                        s: Box::new(s),
                        start: Box::new(start),
                        end: Box::new(end),
                    })
                }
                "split" => {
                    if items.len() != 3 {
                        return Err("split expects (split <s> <sep>)".into());
                    }
                    let s = lower(&items[1], env, ctx)?;
                    let sep = lower(&items[2], env, ctx)?;
                    if ty_of(&s, env, ctx) != Ty::Str || ty_of(&sep, env, ctx) != Ty::Str {
                        return Err("split arguments must be strings".into());
                    }
                    Ok(HExpr::Split {
                        s: Box::new(s),
                        sep: Box::new(sep),
                    })
                }
                "join" => {
                    if items.len() != 3 {
                        return Err("join expects (join <sep> <strs>)".into());
                    }
                    let sep = lower(&items[1], env, ctx)?;
                    if ty_of(&sep, env, ctx) != Ty::Str {
                        return Err("join separator must be a string".into());
                    }
                    let xs = lower(&items[2], env, ctx)?;
                    if ty_of(&xs, env, ctx) != Ty::Vec(Elem::Str) {
                        return Err("join source must be a (Vector str)".into());
                    }
                    Ok(HExpr::Join {
                        sep: Box::new(sep),
                        xs: Box::new(xs),
                    })
                }
                "assoc" => {
                    if items.len() != 4 {
                        return Err("assoc expects (assoc <map> <key> <value>)".into());
                    }
                    let m = lower(&items[1], env, ctx)?;
                    if ty_of(&m, env, ctx) != Ty::Map {
                        return Err("assoc source must be a map".into());
                    }
                    let k = lower(&items[2], env, ctx)?;
                    if ty_of(&k, env, ctx) != Ty::I64 {
                        return Err("map keys must be i64".into());
                    }
                    let v = lower(&items[3], env, ctx)?;
                    if ty_of(&v, env, ctx) != Ty::I64 {
                        return Err("map values must be i64".into());
                    }
                    Ok(HExpr::MapAssoc {
                        m: Box::new(m),
                        k: Box::new(k),
                        v: Box::new(v),
                    })
                }
                "conj" => {
                    if items.len() != 3 {
                        return Err("conj expects (conj <vec> <x>)".into());
                    }
                    let v = lower(&items[1], env, ctx)?;
                    // Sets conj too: (conj s x) adds x (dedup at runtime).
                    if ty_of(&v, env, ctx) == Ty::Set {
                        let x = lower(&items[2], env, ctx)?;
                        if ty_of(&x, env, ctx) != Ty::I64 {
                            return Err("conj element must be i64 for sets".into());
                        }
                        return Ok(HExpr::SetConj {
                            s: Box::new(v),
                            x: Box::new(x),
                        });
                    }
                    let elem = match ty_of(&v, env, ctx) {
                        Ty::Vec(e) => e,
                        other => return Err(format!("conj source must be a vector, got {:?}", other)),
                    };
                    let x = lower(&items[2], env, ctx)?;
                    // Fold: conj on a literal vector with a constant element
                    // is a new static literal — zero runtime cost.
                    if let (HExpr::VecLit(vl), xc) = (&v, &x) {
                        match (vl, xc) {
                            (VLit::I64(vs), HExpr::ConstI64(c)) => {
                                let mut n = vs.clone();
                                n.push(*c);
                                return Ok(HExpr::VecLit(VLit::I64(n)));
                            }
                            (VLit::F64(vs), HExpr::ConstF64(c)) => {
                                let mut n = vs.clone();
                                n.push(*c);
                                return Ok(HExpr::VecLit(VLit::F64(n)));
                            }
                            (VLit::I64(vs), HExpr::ConstF64(c)) => {
                                let mut n = vs.iter().map(|i| *i as f64).collect::<Vec<_>>();
                                n.push(*c);
                                return Ok(HExpr::VecLit(VLit::F64(n)));
                            }
                            _ => {}
                        }
                    }
                    let xt = ty_of(&x, env, ctx);
                    let want = elem_ty(elem);
                    if xt != want && !(want == Ty::F64 && xt == Ty::I64) {
                        return Err(format!("conj element expects {:?}, got {:?}", want, xt));
                    }
                    let x = if want == Ty::F64 && xt == Ty::I64 {
                        HExpr::ConvF64(Box::new(x))
                    } else {
                        x
                    };
                    Ok(HExpr::Conj {
                        v: Box::new(v),
                        x: Box::new(x),
                        elem,
                    })
                }
                "subvec" => {
                    if items.len() != 4 {
                        return Err("subvec expects (subvec <vec> <start> <end>)".into());
                    }
                    let v = lower(&items[1], env, ctx)?;
                    let elem = match ty_of(&v, env, ctx) {
                        Ty::Vec(e) => e,
                        other => return Err(format!("subvec source must be a vector, got {:?}", other)),
                    };
                    let start = lower(&items[2], env, ctx)?;
                    let end = lower(&items[3], env, ctx)?;
                    // Fold: subvec of a literal with constant bounds.
                    if let (HExpr::VecLit(vl), HExpr::ConstI64(s), HExpr::ConstI64(e)) =
                        (&v, &start, &end)
                    {
                        let lo = (*s).max(0) as usize;
                        let hi = (*e).max(0) as usize;
                        match vl {
                            VLit::I64(vs) => {
                                let hi = hi.min(vs.len());
                                let lo = lo.min(hi);
                                return Ok(HExpr::VecLit(VLit::I64(vs[lo..hi].to_vec())));
                            }
                            VLit::F64(vs) => {
                                let hi = hi.min(vs.len());
                                let lo = lo.min(hi);
                                return Ok(HExpr::VecLit(VLit::F64(vs[lo..hi].to_vec())));
                            }
                        }
                    }
                    Ok(HExpr::Subvec {
                        v: Box::new(v),
                        start: Box::new(start),
                        end: Box::new(end),
                        elem,
                    })
                }
                "for" => {
                    if items.len() != 3 {
                        return Err("for expects (for [x src] <body>)".into());
                    }
                    let binding = match &items[1] {
                        Expr::Vector(b) if b.len() == 2 => b.clone(),
                        _ => return Err("for binding must be a 2-element vector [x src]".into()),
                    };
                    let Expr::LitSym(el) = &binding[0] else {
                        return Err("for binder must be a symbol".into());
                    };
                    let src = lower(&binding[1], env, ctx)?;
                    let elem = match ty_of(&src, env, ctx) {
                        Ty::Vec(e) => e,
                        other => return Err(format!("for source must be a vector, got {:?}", other)),
                    };
                    let mut child = env.clone();
                    child.insert(el.clone(), elem_ty(elem));
                    let body = lower(&items[2], &child, ctx)?;
                    // The fill loop is single-block: the body must not fork.
                    if has_ctl(&body) {
                        return Err("for body must be control-flow-free (no if/and/or/match)".into());
                    }
                    let res_elem = match ty_of(&body, &child, ctx) {
                        Ty::I64 => Elem::I64,
                        Ty::F64 => Elem::F64,
                        other => {
                            return Err(format!(
                                "for body must produce i64 or f64 elements, got {:?}",
                                other
                            ))
                        }
                    };
                    Ok(HExpr::For {
                        el: el.clone(),
                        src: Box::new(src),
                        elem,
                        body: Box::new(body),
                        res_elem,
                    })
                }
                "str" => {
                    if items.len() < 2 {
                        return Err("str expects at least one string argument".into());
                    }
                    let mut args = Vec::new();
                    for it in &items[1..] {
                        let a = lower(it, env, ctx)?;
                        if ty_of(&a, env, ctx) != Ty::Str {
                            return Err("str arguments must be strings".into());
                        }
                        args.push(a);
                    }
                    // Fold: concatenating literals is a new literal.
                    if args.iter().all(|a| matches!(a, HExpr::StrLit(_))) {
                        let joined: String = args
                            .iter()
                            .map(|a| {
                                if let HExpr::StrLit(s) = a {
                                    s.clone()
                                } else {
                                    unreachable!()
                                }
                            })
                            .collect();
                        return Ok(HExpr::StrLit(joined));
                    }
                    Ok(HExpr::StrCat(args))
                }
                "clone" => {
                    if items.len() != 2 {
                        return Err("clone expects 1 argument".into());
                    }
                    let e = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Clone(Box::new(e)))
                }
                "drop" => {
                    if items.len() != 2 {
                        return Err("drop expects 1 argument".into());
                    }
                    let e = lower(&items[1], env, ctx)?;
                    Ok(HExpr::DropStmt(Box::new(e)))
                }
                "count" => {
                    if items.len() != 2 {
                        return Err("count expects exactly 1 argument".into());
                    }
                    let inner = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Count(Box::new(inner)))
                }
                "print-string" => {
                    if items.len() != 2 {
                        return Err("print-string expects exactly 1 argument".into());
                    }
                    let a = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Call("print-string".into(), vec![a]))
                }
                "vec-i64" => {
                    let mut vs = Vec::new();
                    for it in &items[1..] {
                        match it {
                            Expr::LitI64(v) => vs.push(*v),
                            _ => return Err("vec-i64 elements must be i64 literals".into()),
                        }
                    }
                    Ok(HExpr::VecLit(VLit::I64(vs)))
                }
                "vec-f64" => {
                    let mut vs = Vec::new();
                    for it in &items[1..] {
                        match it {
                            Expr::LitF64(v) => vs.push(*v),
                            Expr::LitI64(v) => vs.push(*v as f64),
                            _ => {
                                return Err(
                                    "vec-f64 elements must be numeric literals".into()
                                )
                            }
                        }
                    }
                    Ok(HExpr::VecLit(VLit::F64(vs)))
                }
                "print-f64" => {
                    if items.len() != 2 {
                        return Err("print-f64 expects exactly 1 argument".into());
                    }
                    let a = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Call("print-f64".into(), vec![a]))
                }
                "print-i64" => {
                    if items.len() != 2 {
                        return Err("print-i64 expects exactly 1 argument".into());
                    }
                    let a = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Call("print-i64".into(), vec![a]))
                }
                "+" | "-" | "*" | "/" => {
                    if items.len() != 3 {
                        return Err(format!("binary op {} expects 2 operands", head));
                    }
                    let op = match head.as_str() {
                        "+" => BinOp::Add,
                        "-" => BinOp::Sub,
                        "*" => BinOp::Mul,
                        _ => BinOp::Div,
                    };
                    let a = lower(&items[1], env, ctx)?;
                    let b = lower(&items[2], env, ctx)?;
                    return promote_bin(op, a, b, env, ctx);
                }
                _ => {
                    // Constructors resolve before user functions.
                    if let Some((adt, variant, fields)) = ctx.lookup_ctor(head) {
                        if items.len() - 1 != fields.len() {
                            return Err(format!(
                                "constructor {} expects {} argument(s), got {}",
                                head,
                                fields.len(),
                                items.len() - 1
                            ));
                        }
                        let mut args = Vec::new();
                        for (it, fty) in items[1..].iter().zip(fields.iter()) {
                            let a = lower(it, env, ctx)?;
                            let at = ty_of(&a, env, ctx);
                            if *fty == Ty::F64 && at == Ty::I64 {
                                args.push(HExpr::ConvF64(Box::new(a)));
                            } else if at != *fty {
                                return Err(format!(
                                    "constructor {} field expects {:?}, got {:?}",
                                    head, fty, at
                                ));
                            } else {
                                args.push(a);
                            }
                        }
                        return Ok(HExpr::Ctor { adt, variant, args });
                    }
                    let mut args = Vec::new();
                    for it in &items[1..] {
                        args.push(lower(it, env, ctx)?);
                    }
                    // Generic callees are instantiated per call-site type.
                    if let Some(raw) = ctx.raw_by_name.get(head.as_str()) {
                        if !raw.ty_params.is_empty() {
                            let arg_tys: Vec<Ty> =
                                args.iter().map(|a| ty_of(a, env, ctx)).collect();
                            let (mangled, _) = instantiate(head, &arg_tys, ctx)?;
                            return Ok(HExpr::Call(mangled, args));
                        }
                    }
                    Ok(HExpr::Call(head.clone(), args))
                }
            }
        }
    }
}

pub(crate) fn ty_of(e: &HExpr, env: &Env, ctx: &Ctx) -> Ty {
    match e {
        HExpr::ConstI64(_) => Ty::I64,
        HExpr::ConstF64(_) => Ty::F64,
        HExpr::Var(s) => env.get(s).copied().unwrap_or(Ty::I64),
        HExpr::Bin(_, a, b) => {
            if ty_of(a, env, ctx) == Ty::F64 || ty_of(b, env, ctx) == Ty::F64 {
                Ty::F64
            } else {
                Ty::I64
            }
        }
        HExpr::ConvF64(_) => Ty::F64,
        HExpr::Count(_) => Ty::I64,
        HExpr::StrLit(_) => Ty::Str,
        HExpr::Conj { elem, .. } | HExpr::Subvec { elem, .. } => Ty::Vec(*elem),
        HExpr::For { res_elem, .. } => Ty::Vec(*res_elem),
        HExpr::StrCat(_) => Ty::Str,
        HExpr::BinW(.., t) => *t,
        HExpr::CmpW(..) => Ty::Bool,
        HExpr::ICast(_, t) => *t,
        HExpr::MapLit(_) => Ty::Map,
        HExpr::MapAssoc { .. } => Ty::Map,
        HExpr::MapGet { .. } => Ty::I64,
        HExpr::SetLit(_) => Ty::Set,
        HExpr::SetConj { .. } | HExpr::SetDisj { .. } => Ty::Set,
        HExpr::SetContains { .. } => Ty::Bool,
        HExpr::Subs { .. } => Ty::Str,
        HExpr::Split { .. } => Ty::Vec(Elem::Str),
        HExpr::Join { .. } => Ty::Str,
        HExpr::StrCmp(..) => Ty::Bool,
        HExpr::VecLit(vl) => Ty::Vec(match vl {
            VLit::I64(_) => Elem::I64,
            VLit::F64(_) => Elem::F64,
        }),
        HExpr::Reduce(r) => match &**r {
            HReduce::Closure { init, .. } => {
                if matches!(init, HExpr::ConstF64(_)) { Ty::F64 } else { Ty::I64 }
            }
            HReduce::Pipeline { init, .. } => {
                if matches!(init, HExpr::ConstF64(_)) { Ty::F64 } else { Ty::I64 }
            }
        },
        HExpr::Call(name, _) if name == "even?" => Ty::Bool,
        HExpr::Call(name, _) => {
            if let Some(raw) = ctx.raw_by_name.get(name.as_str()) {
                if raw.ty_params.is_empty() {
                    return raw.ret;
                }
            }
            // Monomorphized instantiation: the ret was recorded at lowering.
            ctx.inst
                .borrow()
                .cache
                .get(name)
                .copied()
                .unwrap_or(Ty::I64)
        }
        HExpr::Let(_, b) => ty_of(b, env, ctx),
        HExpr::Seq(v) => v.last().map(|x| ty_of(x, env, ctx)).unwrap_or(Ty::I64),
        HExpr::If(_, _, _, t) => *t,
        HExpr::And(..) | HExpr::Or(..) | HExpr::Not(_) | HExpr::Cmp(..) => Ty::Bool,
        HExpr::Abs(_, t) | HExpr::Min(_, _, t) | HExpr::Max(_, _, t) => *t,
        HExpr::Ctor { adt, .. } => Ty::Adt(*adt),
        HExpr::Match { ty, .. } => *ty,
        HExpr::Get { ty, .. } => *ty,
        HExpr::Clone(inner) => ty_of(inner, env, ctx),
        HExpr::DropStmt(_) => Ty::I64,
    }
}

