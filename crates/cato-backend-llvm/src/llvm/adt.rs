use super::*;

pub(crate) fn adt_of(e: &HExpr, c: &FnCtx, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> u32 {
    match e {
        HExpr::Var(v) => match c.scalar.get(v) {
            Some((_, Ty::Adt(i))) => *i,
            _ => panic!("codegen: {} is not an ADT binding", v),
        },
        HExpr::Ctor { adt, .. } => *adt,
        HExpr::Match { ty: Ty::Adt(i), .. } => *i,
        HExpr::Call(name, _) => match sigs.get(name) {
            Some((Ty::Adt(i), _)) => *i,
            _ => panic!("codegen: call {} does not return an ADT", name),
        },
        other => panic!("codegen: expected an ADT value, got {:?}", other),
    }
}

/// Emit an expression of ADT type; the value is a `ptr` to a stack slot.
pub(crate) fn emit_adt(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(r, _)| r.clone())
            .unwrap_or_else(|| panic!("codegen: unknown ADT var {}", v)),
        HExpr::Ctor { adt, variant, args } => {
            let name = mdl.adt_name(*adt);
            // Alloca hoisted to the entry prelude: safe inside loops, and the
            // value is consumed within the same dynamic extent.
            let slot = c.temp();
            c.prelude
                .push(format!("  {} = alloca {}, align 8", slot, name));
            let t_tg = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 0",
                t_tg, name, slot
            ));
            c.line(&format!("  store i64 {}, ptr {}, align 8", variant, t_tg));
            let fields = mdl.adts[*adt as usize].variants[*variant as usize]
                .1
                .clone();
            for (k, (arg, fty)) in args.iter().zip(fields.iter()).enumerate() {
                let t_f = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                    t_f, name, slot, k
                ));
                match fty {
                    Ty::I64 => {
                        let v = emit_int(arg, c, mdl, sigs);
                        c.line(&format!("  store i64 {}, ptr {}, align 8", v, t_f));
                    }
                    Ty::F64 => {
                        let v = emit_float(arg, c, mdl, sigs);
                        let b = c.temp();
                        c.line(&format!("  {} = bitcast double {} to i64", b, v));
                        c.line(&format!("  store i64 {}, ptr {}, align 8", b, t_f));
                    }
                    Ty::Bool => {
                        let v = emit_bool(arg, c, mdl, sigs);
                        let z = c.temp();
                        c.line(&format!("  {} = zext i1 {} to i64", z, v));
                        c.line(&format!("  store i64 {}, ptr {}, align 8", z, t_f));
                    }
                    other => panic!("codegen: unsupported ADT field type {:?}", other),
                }
            }
            slot
        }
        // clone: materialize a fresh slot and copy the struct by value.
        HExpr::Clone(inner) => {
            let v = emit_adt(inner, c, mdl, sigs);
            let ai = adt_of(inner, c, sigs);
            let name = mdl.adt_name(ai);
            let slot = c.temp();
            c.prelude
                .push(format!("  {} = alloca {}, align 8", slot, name));
            let lv = c.temp();
            c.line(&format!("  {} = load {}, ptr {}, align 8", lv, name, v));
            c.line(&format!("  store {} {}, ptr {}, align 8", name, lv, slot));
            slot
        }
        HExpr::DropStmt(_) => panic!("codegen: drop produces no ADT value"),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Call(..) => call_user_node(e, c, mdl, sigs),
        other => panic!("codegen: expected an ADT expression, got {:?}", other),
    }
}

/// Emit a `match`: switch on the tag, load fields in each arm block, phi the
/// arm results in the merge block (scalars by value, ADTs as ptr).
pub(crate) fn emit_match_node(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::Match { scrut, arms, ty } = e else {
        panic!("codegen: emit_match_node on non-match")
    };
    let scrut_ptr = emit_adt(scrut, c, mdl, sigs);
    let ai = adt_of(scrut, c, sigs);
    let name = mdl.adt_name(ai);

    let t_tg = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 0",
        t_tg, name, scrut_ptr
    ));
    let t_tag = c.temp();
    c.line(&format!("  {} = load i64, ptr {}, align 8", t_tag, t_tg));

    let merged = c.block_name();
    let fail = c.block_name();
    let mut arm_labels: Vec<(Option<u32>, String)> = Vec::new();
    let mut default_lbl = fail.clone();
    for arm in arms {
        let lbl = c.block_name();
        if arm.variant.is_none() {
            // The wildcard arm is the switch default.
            default_lbl = lbl.clone();
        }
        arm_labels.push((arm.variant, lbl));
    }
    c.line(&format!("  switch i64 {}, label %{} [", t_tag, default_lbl));
    for (variant, lbl) in &arm_labels {
        if let Some(vi) = variant {
            c.line(&format!("    i64 {}, label %{}", vi, lbl));
        }
    }
    c.line("]");

    // Default: HIR guarantees exhaustiveness.
    let lbl = c.open(&fail);
    c.line(&format!("{}:", lbl));
    c.line("  unreachable");

    let mut vals: Vec<String> = Vec::new();
    let mut preds: Vec<String> = Vec::new();
    for (arm, (_, lbl)) in arms.iter().zip(arm_labels.iter()) {
        let l = c.open(lbl);
        c.line(&format!("{}:", l));
        if let Some(vi) = arm.variant {
            let fields = mdl.adts[ai as usize].variants[vi as usize]
                .1
                .clone();
            for (k, (binder, fty)) in arm.binders.iter().zip(fields.iter()).enumerate() {
                let t_f = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                    t_f, name, scrut_ptr, k
                ));
                match fty {
                    Ty::I64 => {
                        let t_l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                        c.scalar.insert(binder.clone(), (t_l, Ty::I64));
                    }
                    Ty::F64 => {
                        let t_l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                        let t_d = c.temp();
                        c.line(&format!("  {} = bitcast i64 {} to double", t_d, t_l));
                        c.scalar.insert(binder.clone(), (t_d, Ty::F64));
                    }
                    Ty::Bool => {
                        let t_l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                        let t_b = c.temp();
                        c.line(&format!("  {} = trunc i64 {} to i1", t_b, t_l));
                        c.scalar.insert(binder.clone(), (t_b, Ty::Bool));
                    }
                    other => panic!("codegen: unsupported ADT field type {:?}", other),
                }
            }
        }
        let v = emit_typed(&arm.body, *ty, c, mdl, sigs);
        let v = match ty {
            Ty::Adt(_) => v,
            t => pin_value(
                if *t == Ty::F64 {
                    "double"
                } else if *t == Ty::Bool {
                    "i1"
                } else {
                    "i64"
                },
                &v,
                c,
            ),
        };
        vals.push(v);
        preds.push(c.cur.clone());
        c.line(&format!("  br label %{}", merged));
    }

    let l = c.open(&merged);
    c.line(&format!("{}:", l));
    let res = c.temp();
    let lty = match ty {
        Ty::Adt(_) => "ptr".to_string(),
        Ty::Str => "ptr".to_string(),
        t => mdl.ll_ty(*t),
    };
    let pairs: Vec<String> = vals
        .iter()
        .zip(preds.iter())
        .map(|(v, p)| format!("[ {}, %{} ]", v, p))
        .collect();
    c.line(&format!("  {} = phi {} {}", res, lty, pairs.join(", ")));
    res
}
