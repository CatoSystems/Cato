use super::*;

pub fn build_program(prog: &[Expr]) -> Result<(Vec<AdtDef>, Vec<Defn>), String> {
    build_program_located(
        &prog
            .iter()
            .map(|e| (e.clone(), crate::ast::Span::default()))
            .collect::<Vec<_>>(),
    )
    .map(|(a, d, _, _)| (a, d))
}

/// Build with per-top-level-form spans: parse, lowering and ownership errors
/// are attributed to the enclosing `defn` / `deftype`. Also returns the
/// out-of-line closure bodies and the closure signature table (Phase 14).
pub fn build_program_located(
    prog: &[(Expr, crate::ast::Span)],
) -> Result<
    (
        Vec<AdtDef>,
        Vec<Defn>,
        Vec<crate::Lambda>,
        Vec<(Vec<Ty>, Ty)>,
    ),
    String,
> {
    use crate::diagnostics::locate;
    // Pass 0a: collect deftype headers so field types can reference any
    // declared template regardless of declaration order.
    let mut decl_forms: Vec<(&Expr, &crate::ast::Span)> = Vec::new();
    let mut decls: HashMap<String, (u32, Vec<String>)> = HashMap::new();
    let mut decl_names: Vec<(String, Vec<String>)> = Vec::new();
    for (form, span) in prog.iter() {
        if is_deftype(form) {
            let (name, params) =
                parse_deftype_header(form).map_err(|e| locate(*span, &e))?;
            let idx = decl_names.len() as u32;
            if decls.insert(name.clone(), (idx, params.clone())).is_some() {
                return Err(locate(*span, &format!("duplicate deftype {}", name)));
            }
            decl_names.push((name, params));
            decl_forms.push((form, span));
        }
    }
    // Pass 0b: parse variants with the full template table.
    let mut adts: Vec<AdtDef> = Vec::new();
    for ((form, span), (name, params)) in decl_forms.iter().zip(decl_names.iter()) {
        let variants = parse_deftype_variants(form, name, params, &decls)
            .map_err(|e| locate(**span, &e))?;
        adts.push(AdtDef {
            name: name.clone(),
            ty_params: params.clone(),
            variants,
        });
    }
    let mut adt_by_name: HashMap<String, u32> = HashMap::new();
    for (i, adt) in adts.iter().enumerate() {
        adt_by_name.insert(adt.name.clone(), i as u32);
    }

    // Monomorphized ADT instances accumulate here while parsing annotations
    // and lowering ctor calls; the cell moves into the Ctx for lowering.
    let adt_insts = RefCell::new(AdtInsts::default());
    let tcx = TypeCx {
        adts: &adts,
        adt_by_name: &adt_by_name,
        insts: &adt_insts,
    };

    // Representation facts (boxing/ownership) drive free annotation.
    let facts = crate::hir::compute_adt_facts(&adts);

    // Generic functions (multimethods): collect the method table and
    // strip the forms; call sites resolve during lowering.
    let (prog, gens) = collect_generics(prog, &tcx)?;

    // Pass 1: parse every defn into a raw (un-lowered) form without lowering,
    // so map/filter and calls can resolve any function regardless of order.
    let mut fn_sigs: Vec<(Vec<Ty>, Ty)> = Vec::new();
    let mut raws: Vec<(RawDefn, crate::ast::Span)> = Vec::new();
    for (form, span) in prog.iter() {
        if is_deftype(form) {
            continue;
        }
        let raw = if is_extern(form) {
            parse_extern(form, &tcx, &mut fn_sigs)
        } else {
            parse_raw(form, &tcx, &mut fn_sigs)
        }
        .map_err(|e| locate(*span, &e))?;
        raws.push((raw, *span));
    }
    let raw_by_name: HashMap<&str, &RawDefn> = raws
        .iter()
        .map(|(r, _)| (r.name.as_str(), r))
        .collect();
    let ctx = Ctx {
        raw_by_name: &raw_by_name,
        adts: &adts,
        inst: RefCell::new(InstState::default()),
        adt_insts,
        gens: RefCell::new(gens),
        fn_sigs: RefCell::new(fn_sigs),
        lambdas: RefCell::new(Vec::new()),
    };

    let mut out = Vec::new();
    for (raw, span) in &raws {
        // Generic defns are templates lowered per instantiation; externs
        // are declarations with no body to lower.
        if !raw.ty_params.is_empty() || raw.is_extern {
            continue;
        }
        let mut env = Env::new();
        for (pname, pty) in &raw.params {
            env.insert(pname.clone(), *pty);
        }
        let ret = raw.ret;
        let body = match &raw.body {
            Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
                lower_body_forms(items, &env, &ctx, Some(ret))
            }
            _ => lower_expect(&raw.body, &env, &ctx, Some(ret)),
        }
        .map_err(|e| locate(*span, &e))?;
        out.push(Defn {
            name: raw.name.clone(),
            params: raw.params.clone(),
            ret: raw.ret,
            body,
            borrows: raw.borrows.clone(),
            is_extern: raw.is_extern,
        });
    }
    // FFI declarations: no lowering, all params borrow (C reads them).
    for (raw, _span) in &raws {
        if raw.is_extern {
            out.push(Defn {
                name: raw.name.clone(),
                params: raw.params.clone(),
                ret: raw.ret,
                body: HExpr::ConstI64(0),
                borrows: vec![Borrow::Shared; raw.params.len()],
                is_extern: true,
            });
        }
    }

    // Monomorphized generic instantiations generated during lowering.
    out.extend(ctx.inst.borrow_mut().extra.drain(..));

    // Multimethod hidden/dispatch defns generated during lowering.
    for mut d in ctx.gens.borrow_mut().extra.drain(..) {
        out.push(d);
    }

    // Closure bodies: clone into the program's lambda table.
    let mut lambdas: Vec<crate::Lambda> = ctx
        .lambdas
        .borrow_mut()
        .drain(..)
        .map(|(mangled, captures, params, ret, body)| {
            crate::Lambda {
                mangled,
                captures,
                params,
                ret,
                body,
            }
        })
        .collect();
    // Drain the monomorphized ADT instances; appended below so every
    // Ty::Adt(i) baked into the lowered program indexes the combined table.
    // Template placeholders (instances whose arguments still mention a
    // defn's type parameters, e.g. `(Option T)` in a generic signature)
    // never appear in emitted code; their declarations are inert.
    let extra_adts: Vec<AdtDef> = ctx.adt_insts.borrow_mut().extra.drain(..).collect();
    let fn_sigs = ctx.fn_sigs.borrow().clone();

    let mut adts = adts;
    adts.extend(extra_adts);

    // Ownership / borrow check + scope-exit drop planning: needs every
    // callee's borrow kinds, so it runs once all defns are collected.
    let sigs: HashMap<String, Vec<Borrow>> = out
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    let span_by_name: HashMap<&str, crate::ast::Span> = raws
        .iter()
        .map(|(r, s)| (r.name.as_str(), *s))
        .collect();
    for d in out.iter_mut() {
        if d.is_extern {
            continue;
        }
        plan_drops(&d.name, &d.params, &d.borrows, &mut d.body, &sigs, &facts, false).map_err(|e| {
            let span = span_by_name
                .get(d.name.as_str())
                .copied()
                .unwrap_or_default();
            locate(span, &e)
        })?;
    }
    for lam in lambdas.iter_mut() {
        let name = format!("<lambda {}>", lam.mangled);
        plan_drops(
            &name,
            &lam.params,
            &[],
            &mut lam.body,
            &sigs,
            &facts,
            false,
        )?;
    }
    Ok((adts, out, lambdas, fn_sigs))
}
