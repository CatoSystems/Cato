use super::*;

pub(crate) fn lower_match(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() < 3 {
        return Err("match expects a scrutinee and at least one arm".into());
    }
    // Guards: an arm of the form (V [b :when g] body) falls through to the
    // remaining arms when g fails — desugared into
    //   (V [b] (if g body (match scrut rest...)))
    // plus a routing wildcard `(_ (match scrut rest...))` that keeps the
    // outer match variant-total. Sound because the guard fall-through is
    // only well-defined when the remaining arms cover every variant
    // (checked here by lowering the scrutinee to find its ADT).
    for (ai, arm) in items[2..].iter().enumerate() {
        let Expr::List(arm_items) = arm else { continue };
        let Some(Expr::Vector(bs)) = arm_items.get(1) else { continue };
        let Some(wpos) = bs.iter().position(|b| matches!(b, Expr::LitSym(s) if s == ":when")) else {
            continue;
        };
        if arm_items.len() != 3 || wpos + 2 != bs.len() {
            return Err("guard arms expect (Variant [binders :when cond] body)".into());
        }
        let Some(Expr::LitSym(vname)) = arm_items.first() else {
            return Err("guard arm must start with a variant name".into());
        };
        let rest: Vec<Expr> = items[2 + ai + 1..].to_vec();
        // The scrutinee's ADT: needed to check that the fall-through arms
        // cover every variant.
        let scrut_probe = lower(&items[1], env, ctx)?;
        let Ty::Adt(adt) = ty_of(&scrut_probe, env, ctx) else {
            return Err("guard arms require an ADT scrutinee".into());
        };
        let all: Vec<String> = ctx
            .adt_def(adt)
            .variants
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        let all: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let rest_heads: Vec<&str> = rest
            .iter()
            .filter_map(|r| match r {
                Expr::List(ris) => match ris.first() {
                    Some(Expr::LitSym(h)) => Some(h.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let rest_total = rest_heads.contains(&"_")
            || all.iter().all(|v| rest_heads.contains(v));
        if !rest_total {
            return Err(format!(
                "guard arm {} falls through, but the later arms do not cover every variant (add a _ arm)",
                vname
            ));
        }
        let mut inner: Vec<Expr> = vec![
            Expr::LitSym("match".into()),
            items[1].clone(),
        ];
        inner.extend(rest.clone());
        let rewritten = Expr::List(vec![
            arm_items[0].clone(),
            Expr::Vector(bs[..wpos].to_vec()),
            Expr::List(vec![
                Expr::LitSym("if".into()),
                bs[wpos + 1].clone(),
                arm_items[2].clone(),
                Expr::List(inner.clone()),
            ]),
        ]);
        // Routing wildcard: every variant other than the guarded one goes
        // straight to the fall-through match.
        let route = Expr::List(vec![
            Expr::LitSym("_".into()),
            Expr::List(inner),
        ]);
        let mut next: Vec<Expr> = items[..2 + ai].to_vec();
        next.push(rewritten);
        next.push(route);
        return lower_match(&next, env, ctx);
    }
    let scrut = lower(&items[1], env, ctx)?;
    let Ty::Adt(adt) = ty_of(&scrut, env, ctx) else {
        return Err("match scrutinee must be an ADT value".into());
    };
    let adt_def = ctx.adt_def(adt);

    let mut covered = vec![false; adt_def.variants.len()];
    let mut wildcard = false;
    let mut arm_tys: Vec<Ty> = Vec::new();
    let mut arm_exprs: Vec<(Option<u32>, Vec<String>, HExpr)> = Vec::new();

    for arm in &items[2..] {
        let Expr::List(arm_items) = arm else {
            return Err("match arm must be a list".into());
        };
        let Some(Expr::LitSym(head)) = arm_items.first() else {
            return Err("match arm must start with a variant name or _".into());
        };
        if head == "_" {
            if wildcard {
                return Err("match has more than one wildcard arm".into());
            }
            if arm_items.len() != 2 {
                return Err("wildcard arm expects (_ body)".into());
            }
            wildcard = true;
            let body = lower_expect(&arm_items[1], env, ctx, Some(Ty::Adt(adt)))?;
            arm_tys.push(ty_of(&body, env, ctx));
            arm_exprs.push((None, Vec::new(), body));
            continue;
        }
        let Some((vi, (_, fields))) = adt_def
            .variants
            .iter()
            .enumerate()
            .find(|(_, (n, _))| n == head)
            .map(|(vi, v)| (vi, v.clone()))
        else {
            return Err(format!(
                "{} is not a variant of {}",
                head, adt_def.name
            ));
        };
        if covered[vi] {
            return Err(format!("match covers {} more than once", head));
        }
        covered[vi] = true;
        if arm_items.len() != 3 {
            return Err(format!(
                "match arm {} expects ({} [binders...] body)",
                head, head
            ));
        }
        let Expr::Vector(binders) = &arm_items[1] else {
            return Err(format!("match arm {} binders must be a vector", head));
        };
        if binders.len() != fields.len() {
            return Err(format!(
                "variant {} has {} field(s) but the arm binds {}",
                head,
                fields.len(),
                binders.len()
            ));
        }
        let mut bindernames = Vec::new();
        let mut child = env.clone();
        for (b, fty) in binders.iter().zip(fields.iter()) {
            let Expr::LitSym(bn) = b else {
                return Err(format!("match arm {} binder must be a symbol", head));
            };
            if bn == "_" {
                // Wildcard binder: unique gensym, never referenced by the
                // arm body.
                let g = format!("_wild{}", bindernames.len() + ctx.inst.borrow().cache.len());
                child.insert(g.clone(), *fty);
                bindernames.push(g);
            } else {
                child.insert(bn.clone(), *fty);
                bindernames.push(bn.clone());
            }
        }
            let body = lower_expect(&arm_items[2], &child, ctx, Some(Ty::Adt(adt)))?;
            let bty = ty_of(&body, &child, ctx);
            arm_tys.push(bty);
            arm_exprs.push((Some(vi as u32), bindernames, body));
    }

    // Exhaustiveness: every variant covered, or a wildcard arm present.
    if !wildcard {
        let missing: Vec<&str> = adt_def
            .variants
            .iter()
            .zip(covered.iter())
            .filter(|(_, c)| !**c)
            .map(|((n, _), _)| n.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "match on {} does not cover: {} (add arms or a _ wildcard)",
                adt_def.name,
                missing.join(", ")
            ));
        }
    }

    // All arm bodies must agree on the result type (with the if-rule f64
    // promotion). Coerce int arms up when any arm is f64.
    let mut merged = arm_tys[0];
    for t in &arm_tys[1..] {
        if *t == Ty::F64 || merged == Ty::F64 {
            if *t != merged && *t != Ty::I64 && merged != Ty::I64 {
                return Err(format!(
                    "match arm result types differ: {:?} vs {:?}",
                    merged, t
                ));
            }
            merged = Ty::F64;
        } else if *t != merged {
            return Err(format!(
                "match arm result types differ: {:?} vs {:?}",
                merged, t
            ));
        }
    }

    let mut h_arms = Vec::new();
    for ((variant, binders, body), at) in arm_exprs.into_iter().zip(arm_tys.iter()) {
        let body = coerce_f64(body, *at, merged == Ty::F64);
        h_arms.push(HArm {
            variant,
            binders,
            body,
        });
    }
    Ok(HExpr::Match {
        scrut: Box::new(scrut),
        arms: h_arms,
        ty: merged,
    })
}

