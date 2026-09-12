use super::*;

pub fn build_program(prog: &[Expr]) -> Result<(Vec<AdtDef>, Vec<Defn>), String> {
    // Pass 0: collect deftype forms into the ADT table so every defn (in any
    // order) can refer to the types.
    let mut adts: Vec<AdtDef> = Vec::new();
    for form in prog {
        if is_deftype(form) {
            adts.push(parse_deftype(form)?);
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
    let raws: Vec<RawDefn> = prog
        .iter()
        .filter(|f| !is_deftype(f))
        .map(|f| {
            if is_extern(f) {
                parse_extern(f, &adt_by_name)
            } else {
                parse_raw(f, &adt_by_name)
            }
        })
        .collect::<Result<_, _>>()?;
    let raw_by_name: HashMap<&str, &RawDefn> = raws
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();
    let ctx = Ctx {
        raw_by_name: &raw_by_name,
        adts: &adts,
        inst: RefCell::new(InstState::default()),
    };

    let mut out = Vec::new();
    for raw in &raws {
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
                lower_body_forms(items, &env, &ctx)?
            }
            _ => lower(&raw.body, &env, &ctx)?,
        };
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
    for raw in &raws {
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

    // Ownership / borrow check: needs every callee's borrow kinds.
    let sigs: HashMap<String, Vec<Borrow>> = out
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    for d in &out {
        check_ownership(d, &sigs)?;
    }
    Ok((adts, out))
}

