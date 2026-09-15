use super::*;

pub(crate) fn subst_ty(ty: Ty, subs: &[Option<Ty>], ctx: &Ctx) -> Ty {
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
        Ty::Adt(i) => {
            // A template instance (`(Option T)` in a generic defn's
            // signature): once T is bound, re-instantiate into the concrete
            // instance. Fully concrete instances pass through.
            let tpl_args = ctx
                .adt_insts
                .borrow()
                .rev
                .get(&i)
                .map(|(t, a)| (*t, a.clone()));
            let Some((tpl, args)) = tpl_args else {
                return ty;
            };
            if !args.iter().any(|a| matches!(a, Ty::Param(_))) {
                return ty;
            }
            let new_args: Vec<Ty> = args.iter().map(|a| subst_ty(*a, subs, ctx)).collect();
            match ctx.instantiate_adt(tpl, &new_args) {
                Ok(j) => Ty::Adt(j),
                Err(_) => ty,
            }
        }
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

/// Symbol-safe suffix for mangling a defn instantiation's type arguments
/// (ADT arguments contribute their own mangled instance names).
fn ty_mangle(t: Ty, ctx: &Ctx) -> String {
    match ty_suffix(t) {
        Some(s) => s.to_string(),
        None => match t {
            Ty::Adt(i) => ctx.adt_def(i).name,
            _ => "??".to_string(),
        },
    }
}

/// Unify a declared (possibly generic) type against a concrete one, binding
/// type parameters. Fails (None) when incompatible or unresolved.
pub(crate) fn unify(pat: Ty, actual: Ty, subs: &mut [Option<Ty>], ctx: &Ctx) -> Option<()> {
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
        // A generic ADT parameter (`o :- (Option T)`) unifies with any
        // instance of the same template, field by field.
        (Ty::Adt(i), Ty::Adt(j)) if i != j => unify_adts(i, j, subs, ctx),
        (a, b) if a == b => Some(()),
        _ => None,
    }
}

/// Field-wise unification of two instances of the same ADT template: `i` is
/// the template instance (its fields are the defn's type params), `j` the
/// concrete instance whose fields bind them. Distinct concrete instances of
/// one template do not unify (closed world, no subtyping).
fn unify_adts(i: u32, j: u32, subs: &mut [Option<Ty>], ctx: &Ctx) -> Option<()> {
    let tpl_i = ctx.adt_insts.borrow().rev.get(&i).map(|(t, _)| *t);
    let tpl_j = ctx.adt_insts.borrow().rev.get(&j).map(|(t, _)| *t);
    // Both must be instances of the same declared template.
    if tpl_i.is_none() || tpl_i != tpl_j {
        return None;
    }
    let di = ctx.adt_def(i);
    let dj = ctx.adt_def(j);
    if di.variants.len() != dj.variants.len() {
        return None;
    }
    for ((_, fi), (_, fj)) in di.variants.iter().zip(dj.variants.iter()) {
        if fi.len() != fj.len() {
            return None;
        }
        for (a, b) in fi.iter().zip(fj.iter()) {
            unify(*a, *b, subs, ctx)?;
        }
    }
    Some(())
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
        if unify(*pat, *actual, &mut subs, ctx).is_none() {
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
            .map(|t| ty_mangle(*t, ctx))
            .collect::<Vec<_>>()
            .join("_")
    );
    let ret = subst_ty(raw.ret, &subs, ctx);
    let already = ctx.inst.borrow().cache.contains_key(&mangled);
    if !already {
        // Insert before lowering so recursive calls resolve to this name.
        ctx.inst.borrow_mut().cache.insert(mangled.clone(), ret);
        let mut env = Env::new();
        for (pname, pty) in &raw.params {
            env.insert(pname.clone(), subst_ty(*pty, &subs, ctx));
        }
        let body = lower_body_expr(&raw.body, &env, ctx)?;
        ctx.inst.borrow_mut().extra.push(Defn {
            name: mangled.clone(),
            params: raw
                .params
                .iter()
                .map(|(n, t)| (n.clone(), subst_ty(*t, &subs, ctx)))
                .collect(),
            ret,
            body,
            borrows: raw.borrows.clone(),
            is_extern: false,
        });
    }
    Ok((mangled, ret))
}

