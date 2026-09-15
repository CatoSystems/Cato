use super::*;

pub(crate) fn lambda_spec(
    f: &Expr,
    zip: bool,
    el_ty: Ty,
    el2_ty: Ty,
    env: &Env,
    ctx: &Ctx,
) -> Result<(String, Option<String>, HExpr, Ty), String> {
    let arity = if zip { 2 } else { 1 };
    match f {
        Expr::List(items) => {
            // Inline (fn [el] body) or (fn [el el2] body)
            let Some(Expr::LitSym(h)) = items.first() else {
                return Err("map/filter arg must be a function".into());
            };
            if h != "fn" {
                return Err("map/filter arg must be (fn ...)".into());
            }
            if items.len() < 3 {
                return Err("fn expects (fn [el] <body>)".into());
            }
            let Expr::Vector(params) = &items[1] else {
                return Err("fn params".into());
            };
            if params.len() != arity {
                return Err(format!(
                    "fn must bind exactly {} element param(s) here",
                    arity
                ));
            }
            let el = param_name(&params[0])?;
            let el2 = if zip {
                Some(param_name(&params[1])?)
            } else {
                None
            };
            let mut child = env.clone();
            child.insert(el.clone(), el_ty);
            if let Some(e2) = &el2 {
                child.insert(e2.clone(), el2_ty);
            }
            let body = lower(&items[2], &child, ctx)?;
            let res_ty = ty_of(&body, &child, ctx);
            Ok((el, el2, body, res_ty))
        }
        Expr::LitSym(name) => resolve_function_ref(name, zip, el_ty, el2_ty, env, ctx),
        _ => Err("map/filter arg must be a function".into()),
    }
}


pub(crate) fn resolve_function_ref(
    name: &str,
    zip: bool,
    el_ty: Ty,
    el2_ty: Ty,
    env: &Env,
    ctx: &Ctx,
) -> Result<(String, Option<String>, HExpr, Ty), String> {
    if name == "even?" {
        if zip {
            return Err("even? cannot be used over a zip source".into());
        }
        return Ok((
            "x".to_string(),
            None,
            HExpr::Call("even?".to_string(), vec![HExpr::Var("x".to_string())]),
            Ty::Bool,
        ));
    }
    let raw = ctx
        .raw_by_name
        .get(name)
        .ok_or_else(|| format!("unknown function in map/filter: {}", name))?;
    let arity = if zip { 2 } else { 1 };
    if raw.params.len() != arity {
        return Err(format!(
            "function {} used in map/filter must have {} param(s)",
            name, arity
        ));
    }
    // Generic functions used as stage functions are resolved against the
    // element type; their declared return type is substituted accordingly.
    let subs = if raw.ty_params.is_empty() {
        Vec::new()
    } else {
        let mut subs: Vec<Option<Ty>> = vec![None; raw.ty_params.len()];
        // The stage function's params are element patterns: unify the
        // vector element with `(Vector T)`-shaped params, scalars directly.
        for (i, (_, pat)) in raw.params.iter().enumerate() {
            let actual = match i {
                0 => el_ty,
                _ => el2_ty,
            };
            let actual = match pat {
                Ty::Vec(_) => Ty::Vec(match actual {
                    Ty::I64 => Elem::I64,
                    Ty::F64 => Elem::F64,
                    other => {
                        return Err(format!(
                            "cannot use {} over {:?} elements in map/filter",
                            name, other
                        ))
                    }
                }),
                _ => actual,
            };
            unify(*pat, actual, &mut subs, ctx)
                .ok_or_else(|| format!("cannot use {} over {:?} elements", name, actual))?;
        }
        subs
    };
    let el = raw.params[0].0.clone();
    let el2 = if zip {
        Some(raw.params[1].0.clone())
    } else {
        None
    };
    let mut child = env.clone();
    child.insert(el.clone(), el_ty);
    if let Some(e2) = &el2 {
        child.insert(e2.clone(), el2_ty);
    }
    let body = lower_body_expr(&raw.body, &child, ctx, None)?;
    let ret = if subs.is_empty() {
        raw.ret
    } else {
        subst_ty(raw.ret, &subs, ctx)
    };
    Ok((el, el2, body, ret))
}


