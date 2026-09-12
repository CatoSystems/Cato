use super::*;

pub(crate) fn lower_match(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() < 3 {
        return Err("match expects a scrutinee and at least one arm".into());
    }
    let scrut = lower(&items[1], env, ctx)?;
    let Ty::Adt(adt) = ty_of(&scrut, env, ctx) else {
        return Err("match scrutinee must be an ADT value".into());
    };
    let adt_def = &ctx.adts[adt as usize];

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
            let body = lower(&arm_items[1], env, ctx)?;
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
            child.insert(bn.clone(), *fty);
            bindernames.push(bn.clone());
        }
            let body = lower(&arm_items[2], &child, ctx)?;
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

