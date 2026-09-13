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
    // Pass 0: collect deftype forms into the ADT table so every defn (in any
    // order) can refer to the types.
    let mut adts: Vec<AdtDef> = Vec::new();
    for (form, span) in prog {
        if is_deftype(form) {
            adts.push(parse_deftype(form).map_err(|e| locate(*span, &e))?);
        }
    }
    let mut adt_by_name: HashMap<String, u32> = HashMap::new();
    for (i, adt) in adts.iter().enumerate() {
        if adt_by_name.insert(adt.name.clone(), i as u32).is_some() {
            return Err(format!("duplicate deftype {}", adt.name));
        }
    }

    // Pass 1: parse every defn into a raw (un-lowered) form without lowering,
    // so map/filter and calls can resolve any function regardless of order.
    let mut fn_sigs: Vec<(Vec<Ty>, Ty)> = Vec::new();
    let mut raws: Vec<(RawDefn, crate::ast::Span)> = Vec::new();
    for (form, span) in prog {
        if is_deftype(form) {
            continue;
        }
        let raw = if is_extern(form) {
            parse_extern(form, &adt_by_name, &mut fn_sigs)
        } else {
            parse_raw(form, &adt_by_name, &mut fn_sigs)
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
        let body = match &raw.body {
            Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
                lower_body_forms(items, &env, &ctx)
            }
            _ => lower(&raw.body, &env, &ctx),
        }
        .map_err(|e| locate(*span, &e))?;
        let mut body = body;
        annotate_frees(&mut body);
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

    // Closure bodies: clone into the program's lambda table.
    let lambdas: Vec<crate::Lambda> = ctx
        .lambdas
        .borrow_mut()
        .drain(..)
        .map(|(mangled, captures, params, ret, mut body)| {
            annotate_frees(&mut body);
            crate::Lambda {
                mangled,
                captures,
                params,
                ret,
                body,
            }
        })
        .collect();
    let fn_sigs = ctx.fn_sigs.borrow().clone();

    // Ownership / borrow check: needs every callee's borrow kinds.
    let sigs: HashMap<String, Vec<Borrow>> = out
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    let span_by_name: HashMap<&str, crate::ast::Span> = raws
        .iter()
        .map(|(r, s)| (r.name.as_str(), *s))
        .collect();
    for d in &out {
        check_ownership(d, &sigs).map_err(|e| {
            let span = span_by_name
                .get(d.name.as_str())
                .copied()
                .unwrap_or_default();
            locate(span, &e)
        })?;
    }
    Ok((adts, out, lambdas, fn_sigs))
}
