use super::*;

pub(crate) fn subst_ty(ty: Ty, subs: &[Option<Ty>]) -> Ty {
    match ty {
        Ty::Param(i) => subs
            .get(i as usize)
            .copied()
            .flatten()
            .unwrap_or(Ty::I64),
        Ty::Vec(e) => Ty::Vec(match e {
            // `(Vector T)`: T was bound to the element type at unification.
            Elem::Param(i) => match subs.get(i as usize).copied().flatten() {
                Some(Ty::I64) => Elem::I64,
                Some(Ty::F64) => Elem::F64,
                Some(Ty::Vec(el)) => el,
                _ => Elem::I64,
            },
            other => other,
        }),
        other => other,
    }
}

/// LLVM-safe suffix for an instantiation's type arguments.
pub(crate) fn ty_suffix(ty: Ty) -> Option<&'static str> {
    match ty {
        Ty::I64 => Some("i64"),
        Ty::F64 => Some("f64"),
        Ty::Bool => Some("bool"),
        Ty::Vec(Elem::I64) => Some("vec_i64"),
        Ty::Vec(Elem::F64) => Some("vec_f64"),
        Ty::Vec(Elem::Str) => Some("vec_str"),
        _ => None,
    }
}

/// Unify a declared (possibly generic) type against a concrete one, binding
/// type parameters. Fails (None) when incompatible or unresolved.
pub(crate) fn unify(pat: Ty, actual: Ty, subs: &mut [Option<Ty>]) -> Option<()> {
    match (pat, actual) {
        (Ty::Param(i), _) => {
            match subs.get_mut(i as usize)? {
                Some(bound) if *bound != actual => return None,
                slot => *slot = Some(actual),
            }
            Some(())
        }
        // `(Vector T)` binds T to the element type, not the vector type.
        (Ty::Vec(Elem::Param(i)), Ty::Vec(el)) => {
            let a = match el {
                Elem::I64 => Ty::I64,
                Elem::F64 => Ty::F64,
                Elem::Str => Ty::Str,
                Elem::Param(_) => return None,
            };
            match subs.get_mut(i as usize)? {
                Some(bound) if *bound != a => return None,
                slot => *slot = Some(a),
            }
            Some(())
        }
        (Ty::Vec(Elem::Param(_)), _) => None,
        (a, b) if a == b => Some(()),
        _ => None,
    }
}

/// Resolve an instantiation of a (possibly generic) callee for the given
/// concrete argument types. Returns the (mangled) callee name and its
/// concrete return type. Non-generic callees pass through untouched.
pub(crate) fn instantiate(
    name: &str,
    arg_tys: &[Ty],
    ctx: &Ctx,
) -> Result<(String, Ty), String> {
    let raw = match ctx.raw_by_name.get(name) {
        Some(r) => r,
        None => return Err(format!("unknown function {}", name)),
    };
    if raw.ty_params.is_empty() {
        return Ok((name.to_string(), raw.ret));
    }
    if raw.params.len() != arg_tys.len() {
        return Err(format!(
            "function {} expects {} argument(s), got {}",
            name,
            raw.params.len(),
            arg_tys.len()
        ));
    }
    let mut subs: Vec<Option<Ty>> = vec![None; raw.ty_params.len()];
    for ((_, pat), actual) in raw.params.iter().zip(arg_tys.iter()) {
        if unify(*pat, *actual, &mut subs).is_none() {
            return Err(format!(
                "cannot instantiate {} with argument types {:?}",
                name, arg_tys
            ));
        }
    }
    let ty_args: Vec<Ty> = raw
        .ty_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            subs.get(i).copied().flatten().ok_or_else(|| {
                format!("cannot infer type parameter {} of {}", p, name)
            })
        })
        .collect::<Result<_, _>>()?;
    let mangled = format!(
        "{}__{}",
        name,
        ty_args
            .iter()
            .map(|t| ty_suffix(*t).unwrap_or("??"))
            .collect::<Vec<_>>()
            .join("_")
    );
    let ret = subst_ty(raw.ret, &subs);
    let already = ctx.inst.borrow().cache.contains_key(&mangled);
    if !already {
        // Insert before lowering so recursive calls resolve to this name.
        ctx.inst.borrow_mut().cache.insert(mangled.clone(), ret);
        let mut env = Env::new();
        for (pname, pty) in &raw.params {
            env.insert(pname.clone(), subst_ty(*pty, &subs));
        }
        let body = lower_body_expr(&raw.body, &env, ctx)?;
        ctx.inst.borrow_mut().extra.push(Defn {
            name: mangled.clone(),
            params: raw
                .params
                .iter()
                .map(|(n, t)| (n.clone(), subst_ty(*t, &subs)))
                .collect(),
            ret,
            body,
            borrows: raw.borrows.clone(),
            is_extern: false,
        });
    }
    Ok((mangled, ret))
}

