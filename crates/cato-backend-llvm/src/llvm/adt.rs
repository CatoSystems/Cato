use super::*;

pub(crate) fn adt_of(e: &HExpr, c: &FnCtx, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> u32 {
    match e {
        HExpr::Var(v) => match c.scalar.get(v) {
            Some((_, Ty::Adt(i))) => *i,
            _ => panic!("codegen: {} is not an ADT binding", v),
        },
        HExpr::Ctor { adt, .. } => *adt,
        HExpr::Match { ty: Ty::Adt(i), .. } => *i,
        HExpr::If(_, _, _, Ty::Adt(i)) => *i,
        // A clone of an ADT value has that ADT's representation; emit_adt
        // handles the copy itself (boxed helper or inline struct load).
        HExpr::Clone(inner) => adt_of(inner, c, sigs),
        HExpr::Call(name, _) => match sigs.get(name) {
            Some((Ty::Adt(i), _)) => *i,
            _ => panic!("codegen: call {} does not return an ADT", name),
        },
        // A seq of effects ending in the value: the tail decides (nothing
        // inside a Seq frees, so the scrutinee pointer stays valid through
        // the match). A `let` scrutinee is rejected by the checker — its
        // scope-exit free would run inside emit_adt, before the arms.
        HExpr::Seq(vs) => match vs.last() {
            Some(last) => adt_of(last, c, sigs),
            None => panic!("codegen: empty seq has no ADT value"),
        },
        // The planner's own `let` around a scrutinee call: its bindings are
        // hoisted argument temps, freed before the arms, and the value is
        // the call's.
        HExpr::Let(_, body, _) => adt_of(body, c, sigs),
        // The loop's recorded completing type.
        HExpr::Loop { ty: Ty::Adt(i), .. } => *i,
        HExpr::StrNth { res: Some(Ty::Adt(i)), .. }
        | HExpr::Get { res: Some(Ty::Adt(i)), .. }
        | HExpr::MapGet { res: Some(Ty::Adt(i)), .. } => *i,
        // parse-i64/parse-f64: the runtime's Option box, typed by `res`.
        HExpr::ParseNum { res: Ty::Adt(i), .. } => *i,
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
        // Option-returning lookups are inline ADT values in any position
        // (e.g. a match scrutinee).
        HExpr::StrNth { s, idx, res: Some(res_ty) } => {
            if let Ty::Adt(adt) = res_ty {
                return emit_strnth_opt(s, idx, *adt, c, mdl, sigs);
            }
            panic!("codegen: StrNth Option result must be an ADT")
        }
        HExpr::Get { src, idx, res: Some(res_ty), .. } => {
            if let Ty::Adt(adt) = res_ty {
                return emit_get_opt(src, idx, *adt, c, mdl, sigs);
            }
            panic!("codegen: Get Option result must be an ADT")
        }
        HExpr::MapGet { src, key, res: Some(res_ty), .. } => {
            if let Ty::Adt(adt) = res_ty {
                return emit_map_get_opt(src, key, *adt, c, mdl, sigs);
            }
            panic!("codegen: MapGet Option result must be an ADT")
        }
        HExpr::Ctor { adt, variant, args } => {
            let name = mdl.adt_name(*adt);
            let boxed = mdl.adt_boxed(*adt);
            // Boxed ADTs allocate their struct on the heap; the value is the
            // owning pointer. Inline ADTs alloca an entry slot (hoisted:
            // safe inside loops, consumed within the same dynamic extent).
            let slot = if boxed {
                emit_malloc(c, mdl, &format!("{}", 8 * (1 + mdl.adt_slots(*adt))))
            } else {
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca {}, align 8", slot, name));
                slot
            };
            let t_tg = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 0",
                t_tg, name, slot
            ));
            c.line(&format!("  store i64 {}, ptr {}, align 8", variant, t_tg));
            let def = mdl.adts[*adt as usize].clone();
            let offsets = &mdl.adt_field_offsets(*adt)[*variant as usize];
            for (k, (arg, fty)) in args.iter().zip(def.variants[*variant as usize].1.iter()).enumerate() {
                emit_field_store(arg, fty, offsets[k], name.clone(), slot.clone(), c, mdl, sigs);
            }
            slot
        }
        // clone: a boxed ADT deep-copies through its generated helper (the
        // copy owns its children); an inline ADT copies the struct by value.
        HExpr::Clone(inner) => {
            let ai = adt_of(inner, c, sigs);
            if mdl.adt_boxed(ai) {
                let v = emit_adt(inner, c, mdl, sigs);
                let r = c.temp();
                c.line(&format!(
                    "  {} = call ptr @clone__{}(ptr {})",
                    r,
                    mdl.adts[ai as usize].name,
                    v
                ));
                return r;
            }
            let v = emit_adt(inner, c, mdl, sigs);
            let name = mdl.adt_name(ai);
            let slot = c.temp();
            c.prelude
                .push(format!("  {} = alloca {}, align 8", slot, name));
            let lv = c.temp();
            c.line(&format!("  {} = load {}, ptr {}, align 8", lv, name, v));
            c.line(&format!("  store {} {}, ptr {}, align 8", name, lv, slot));
            slot
        }
        HExpr::If(cond, then, els, Ty::Adt(i)) => {
            emit_if_node(cond, then, els, Ty::Adt(*i), c, mdl, sigs)
        }
        HExpr::Let(binds, body, frees) => {
            emit_let_scoped(binds, frees, c, mdl, sigs, |c, mdl| emit_adt(body, c, mdl, sigs))
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_adt(vs.last().unwrap(), c, mdl, sigs)
        }
        HExpr::DropStmt(_) => panic!("codegen: drop produces no ADT value"),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Call(..) => call_user_node(e, c, mdl, sigs),
        // A loop completing with a boxed ADT (the reader's scan returns the
        // RRes from its `loop`): the loop machinery returns the completing
        // register — an owning ptr for boxed results.
        HExpr::Loop { .. } => emit_loop_node(e, c, mdl, sigs),
        // parse-i64/parse-f64 in value position (e.g. a match scrutinee):
        // the runtime's 16-byte Option box, a ptr to the inline
        // {tag, payload} layout the match consumes.
        HExpr::ParseNum { s, int, .. } => {
            let sv = emit_str_val(s, c, mdl, sigs);
            let (decl, helper) = if *int {
                ("declare ptr @cato_parse_i64(ptr)", "cato_parse_i64")
            } else {
                ("declare ptr @cato_parse_f64(ptr)", "cato_parse_f64")
            };
            mdl.ensure_decl(decl);
            let p = c.temp();
            c.line(&format!("  {} = call ptr @{}(ptr {})", p, helper, sv));
            p
        }
        other => panic!("codegen: expected an ADT expression, got {:?}", other),
    }
}

/// The planner's arm-exit free (`HArm::exit_free`): once the arm's value is
/// computed, null the field slots of the binders whose objects left (moved
/// into a callee or constructor, or the arm's own value), then run the
/// box's drop glue — null-tolerant, so exactly the objects nobody else
/// holds are released. Collection fields null all three header words so a
/// skipped `(Vector str)` frees neither its array nor its elements.
pub(crate) fn emit_arm_exit_free(
    arm: &HArm,
    scrut_ptr: &str,
    ai: u32,
    name: &str,
    c: &mut FnCtx,
    mdl: &mut Mdl,
) {
    let Some(skip) = &arm.exit_free else {
        return;
    };
    emit_exit_free_parts(skip, arm.variant, &arm.binders, scrut_ptr, ai, name, c, mdl);
}

/// A pending arm exit, run at a `recur` that leaves the arm.
pub(crate) fn emit_pending_exit(pe: &PendingExit, c: &mut FnCtx, mdl: &mut Mdl) {
    emit_exit_free_parts(&pe.skip, pe.variant, &pe.binders, &pe.scrut_ptr, pe.ai, &pe.name, c, mdl);
}

#[allow(clippy::too_many_arguments)]
fn emit_exit_free_parts(
    skip: &[String],
    variant: Option<u32>,
    binders: &[String],
    scrut_ptr: &str,
    ai: u32,
    name: &str,
    c: &mut FnCtx,
    mdl: &mut Mdl,
) {
    if !mdl.adt_owns(ai) {
        return;
    }
    if let Some(vi) = variant {
        if !skip.is_empty() {
            let def = mdl.adts[ai as usize].clone();
            let fields = def.variants[vi as usize].1.clone();
            let offsets = mdl.adt_field_offsets(ai)[vi as usize].clone();
            for (k, (binder, fty)) in binders.iter().zip(fields.iter()).enumerate() {
                if !skip.contains(binder) {
                    continue;
                }
                let n = cato_core::field_slot_count(fty, &mdl.boxed, &mdl.adts);
                for s in 0..n {
                    let t_f = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                        t_f,
                        name,
                        scrut_ptr,
                        offsets[k] + s
                    ));
                    c.line(&format!("  store i64 0, ptr {}, align 8", t_f));
                }
            }
        }
    }
    c.line(&format!(
        "  call void @drop__{}(ptr {})",
        mdl.adts[ai as usize].name, scrut_ptr
    ));
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
        let scope = c.save_binds(&arm.binders);
        if let Some(vi) = arm.variant {
            let def = mdl.adts[ai as usize].clone();
            let fields = def.variants[vi as usize].1.clone();
            let offsets = &mdl.adt_field_offsets(ai)[vi as usize];
            for (k, (binder, fty)) in arm.binders.iter().zip(fields.iter()).enumerate() {
                emit_field_load(binder, fty, offsets[k], ai, name.clone(), scrut_ptr.clone(), c, mdl);
            }
        }
        let v = emit_typed(&arm.body, *ty, c, mdl, sigs);
        emit_arm_exit_free(arm, &scrut_ptr, ai, &name, c, mdl);
        // The arm's binders end here: a sibling arm referencing a shadowed
        // name must see the outer binding, not this arm's register.
        c.restore_binds(scope);
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

/// A `match` on a loop's tail path: arm bodies go through the recur-aware
/// `emit_loop_tail`, so an arm whose body branches back to the loop head
/// contributes no phi input. Returns `None` when every arm recurs — the
/// merge is unreachable and the loop continues.
pub(crate) fn emit_match_in_loop(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
    loop_tail: &dyn Fn(&HExpr, Ty, &mut FnCtx, &mut Mdl, &HashMap<String, (Ty, Vec<Ty>)>) -> Option<String>,
) -> Option<String> {
    let HExpr::Match { scrut, arms, ty } = e else {
        panic!("codegen: emit_match_in_loop on non-match")
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

    let lbl = c.open(&fail);
    c.line(&format!("{}:", lbl));
    c.line("  unreachable");

    let mut vals: Vec<String> = Vec::new();
    let mut preds: Vec<String> = Vec::new();
    for (arm, (_, lbl)) in arms.iter().zip(arm_labels.iter()) {
        let l = c.open(lbl);
        c.line(&format!("{}:", l));
        let scope = c.save_binds(&arm.binders);
        if let Some(vi) = arm.variant {
            let def = mdl.adts[ai as usize].clone();
            let fields = def.variants[vi as usize].1.clone();
            let offsets = &mdl.adt_field_offsets(ai)[vi as usize];
            for (k, (binder, fty)) in arm.binders.iter().zip(fields.iter()).enumerate() {
                emit_field_load(binder, fty, offsets[k], ai, name.clone(), scrut_ptr.clone(), c, mdl);
            }
        }
        // A `recur` inside this arm leaves it: register the arm's exit
        // free so the jump runs it (after the recur arguments).
        let pushed = if let Some(skip) = &arm.exit_free {
            c.pending_exits.push(PendingExit {
                skip: skip.clone(),
                variant: arm.variant,
                binders: arm.binders.clone(),
                scrut_ptr: scrut_ptr.clone(),
                ai,
                name: name.clone(),
            });
            true
        } else {
            false
        };
        let v = loop_tail(&arm.body, *ty, c, mdl, sigs);
        if pushed {
            c.pending_exits.pop();
        }
        if v.is_some() {
            emit_arm_exit_free(arm, &scrut_ptr, ai, &name, c, mdl);
        }
        c.restore_binds(scope);
        if let Some(v) = v {
            let v = match ty {
                Ty::Adt(_) | Ty::Vec(_) | Ty::Map | Ty::Set => v,
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
    }

    if vals.is_empty() {
        // Every arm branched back to the loop head; no value reaches here.
        return None;
    }
    let l = c.open(&merged);
    c.line(&format!("{}:", l));
    let res = c.temp();
    let lty = match ty {
        Ty::Adt(_) | Ty::Str => "ptr".to_string(),
        Ty::Vec(_) | Ty::Map | Ty::Set => "%VecI64".to_string(),
        t => mdl.ll_ty(*t),
    };
    let pairs: Vec<String> = vals
        .iter()
        .zip(preds.iter())
        .map(|(v, p)| format!("[ {}, %{} ]", v, p))
        .collect();
    c.line(&format!("  {} = phi {} {}", res, lty, pairs.join(", ")));
    Some(res)
}

// ---------------------------------------------------------------------------
// S1b: widened field layout — stores, loads, and deep-clone helpers
// ---------------------------------------------------------------------------

/// Store one constructor argument into its slot offset. Strings and boxed
/// ADT children go through ptrtoint (the payload is [N x i64]); vectors
/// spread across three slots; inline ADT children are copied whole.
pub(crate) fn emit_field_store(
    arg: &HExpr,
    fty: &Ty,
    off: usize,
    name: String,
    slot: String,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) {
    let t_f = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
        t_f, name, slot, off
    ));
    match fty {
        Ty::I64 => {
            let v = emit_int(arg, c, mdl, sigs);
            c.line(&format!("  store i64 {}, ptr {}, align 8", v, t_f));
        }
        Ty::I32 => {
            let v = emit_int(arg, c, mdl, sigs);
            let x = c.temp();
            c.line(&format!("  {} = sext i32 {} to i64", x, v));
            c.line(&format!("  store i64 {}, ptr {}, align 8", x, t_f));
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
        Ty::Str => {
            // The ADT owns its own copy of the bytes (strdup), so the
            // source binding can be freed without dangling the field.
            let v = emit_str_val(arg, c, mdl, sigs);
            mdl.ensure_decl("declare ptr @strdup(ptr)");
            let copy = c.temp();
            c.line(&format!("  {} = call ptr @strdup(ptr {})", copy, v));
            let x = c.temp();
            c.line(&format!("  {} = ptrtoint ptr {} to i64", x, copy));
            c.line(&format!("  store i64 {}, ptr {}, align 8", x, t_f));
        }
        Ty::Vec(elem) => {
            // The ADT owns its own copy of the buffer (the str strdup rule,
            // widened to vectors): the source may be static data or an
            // alias, so the constructor always deep-copies.
            let (p, l, _cap) = emit_vec_header(arg, c, mdl, sigs);
            let esz: i64 = match elem {
                Elem::F64 => 8,
                _ => 8,
            };
            let nlen = c.temp();
            c.line(&format!("  {} = mul i64 {}, {}", nlen, l, esz));
            mdl.ensure_decl("declare ptr @malloc(i64)");
            let np = c.temp();
            c.line(&format!("  {} = call ptr @malloc(i64 {})", np, nlen));
            mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
            c.line(&format!(
                "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
                np, p, nlen
            ));
            let caps = vec![np, l.clone(), l];
            for (k, v) in caps.into_iter().enumerate() {
                let t_k = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                    t_k,
                    name,
                    slot,
                    off + k
                ));
                let vv = if k == 0 {
                    let x = c.temp();
                    c.line(&format!("  {} = ptrtoint ptr {} to i64", x, v));
                    x
                } else {
                    v
                };
                c.line(&format!("  store i64 {}, ptr {}, align 8", vv, t_k));
            }
        }
        Ty::Adt(j) => {
            let v = emit_adt(arg, c, mdl, sigs);
            if mdl.adt_boxed(*j) {
                // One pointer slot to the owning child.
                let x = c.temp();
                c.line(&format!("  {} = ptrtoint ptr {} to i64", x, v));
                c.line(&format!("  store i64 {}, ptr {}, align 8", x, t_f));
            } else {
                // Inline child: copy the whole embedded struct.
                let child = mdl.adt_name(*j);
                let lv = c.temp();
                c.line(&format!("  {} = load {}, ptr {}, align 8", lv, child, v));
                c.line(&format!("  store {} {}, ptr {}, align 8", child, lv, t_f));
            }
        }
        other => panic!("codegen: unsupported ADT field type {:?}", other),
    }
}

/// Load one field at its slot offset and bind the arm's binder. Boxed ADT
/// children come back as plain owning pointers; inline children bind a
/// pointer into the parent struct (values are immutable, so aliasing the
/// embedded region is safe); vectors rebuild a header slot.
pub(crate) fn emit_field_load(
    binder: &str,
    fty: &Ty,
    off: usize,
    ai: u32,
    name: String,
    scrut_ptr: String,
    c: &mut FnCtx,
    mdl: &mut Mdl,
) {
    let t_f = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
        t_f, name, scrut_ptr, off
    ));
    match fty {
        Ty::I64 => {
            let t_l = c.temp();
            c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
            c.scalar.insert(binder.to_string(), (t_l, Ty::I64));
        }
        Ty::I32 => {
            let t_l = c.temp();
            c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
            let t_i = c.temp();
            c.line(&format!("  {} = trunc i64 {} to i32", t_i, t_l));
            c.scalar.insert(binder.to_string(), (t_i, Ty::I32));
        }
        Ty::F64 => {
            let t_l = c.temp();
            c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
            let t_d = c.temp();
            c.line(&format!("  {} = bitcast i64 {} to double", t_d, t_l));
            c.scalar.insert(binder.to_string(), (t_d, Ty::F64));
        }
        Ty::Bool => {
            let t_l = c.temp();
            c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
            let t_b = c.temp();
            c.line(&format!("  {} = trunc i64 {} to i1", t_b, t_l));
            c.scalar.insert(binder.to_string(), (t_b, Ty::Bool));
        }
        Ty::Str => {
            let t_l = c.temp();
            c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
            let t_p = c.temp();
            c.line(&format!("  {} = inttoptr i64 {} to ptr", t_p, t_l));
            c.scalar.insert(binder.to_string(), (t_p, Ty::Str));
        }
        Ty::Vec(elem) => {
            // Only the alloca hoists to the entry block; the loads read the
            // boxed struct, which is valid only inside this arm's block.
            let slot = c.temp();
            c.prelude
                .push(format!("  {} = alloca %VecI64, align 8", slot));
            for k in 0..3usize {
                let t_k = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr {}, i32 0, i32 1, i32 {}",
                    t_k, name, scrut_ptr,
                    off + k
                ));
                let t_l = c.temp();
                c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_k));
                let t_dst = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 {}",
                    t_dst, slot, k
                ));
                if k == 0 {
                    let t_p = c.temp();
                    c.line(&format!("  {} = inttoptr i64 {} to ptr", t_p, t_l));
                    c.line(&format!("  store ptr {}, ptr {}, align 8", t_p, t_dst));
                } else {
                    c.line(&format!("  store i64 {}, ptr {}, align 8", t_l, t_dst));
                }
            }
            c.grow.insert(binder.to_string(), (slot, *elem));
        }
        Ty::Adt(j) => {
            if mdl.adt_boxed(*j) {
                let t_l = c.temp();
                c.line(&format!("  {} = load i64, ptr {}, align 8", t_l, t_f));
                let t_p = c.temp();
                c.line(&format!("  {} = inttoptr i64 {} to ptr", t_p, t_l));
                c.scalar.insert(binder.to_string(), (t_p, Ty::Adt(*j)));
            } else {
                // Bind a pointer into the parent's embedded region.
                c.scalar
                    .insert(binder.to_string(), (t_f, Ty::Adt(*j)));
            }
            let _ = ai;
        }
        other => panic!("codegen: unsupported ADT field type {:?}", other),
    }
}


/// A heap copy of a possibly-static vector: str elements strdup via
/// cato_str_vec_copy, numerics memcpy into a fresh same-length buffer.
fn heap_copy_vec(
    p: &str,
    l: &str,
    elem: Elem,
    c: &mut FnCtx,
    mdl: &mut Mdl,
) -> String {
    if elem == Elem::Str {
        mdl.ensure_decl("declare ptr @cato_str_vec_copy(ptr, i64, i64)");
        let d = c.temp();
        c.line(&format!(
            "  {} = call ptr @cato_str_vec_copy(ptr {}, i64 {}, i64 {})",
            d, p, l, l
        ));
        d
    } else {
        let nbytes = c.temp();
        c.line(&format!("  {} = mul i64 {}, {}", nbytes, l, elem.bytes()));
        let d = emit_malloc(c, mdl, &nbytes);
        mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
        c.line(&format!(
            "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
            d, p, nbytes
        ));
        d
    }
}

/// Materialize a vector argument as (ptr, len, cap) for field storage.
/// Field storage snapshots the header, so uniquely-owned growth slots load
/// their live header; static literals report cap = len (they never grow).
pub(crate) fn emit_vec_header(
    arg: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String, String) {
    match arg {
        HExpr::Var(v) => {
            if c.grow.contains_key(v) {
                let (slot, _) = c.grow.get(v).cloned().unwrap();
                let mut out = Vec::new();
                for k in 0..3usize {
                    let f = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 {}",
                        f, slot, k
                    ));
                    if k == 0 {
                        let l = c.temp();
                        c.line(&format!("  {} = load ptr, ptr {}, align 8", l, f));
                        out.push(l);
                    } else {
                        let l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", l, f));
                        out.push(l);
                    }
                }
                let cap = out.pop().unwrap();
                let l = out.pop().unwrap();
                let p = out.pop().unwrap();
                (p, l, cap)
            } else {
                let (p, l, elem) = c
                    .vecs
                    .get(v)
                    .cloned()
                    .unwrap_or_else(|| panic!("codegen: {} is not a vector binding", v));
                // A binding whose data pointer is a static global (an
                // empty/vector literal) must not reach owning fields: the
                // drop glue would free the global.
                if p.starts_with('@') {
                    let dp = heap_copy_vec(&p, &l, elem, c, mdl);
                    (dp, l.clone(), l)
                } else {
                    (p, l.clone(), l)
                }
            }
        }
        HExpr::VecLit(_) => {
            // Field storage must own its buffer: literals mint static
            // globals, which the owning ADT's drop glue would free. Give
            // the field a heap copy (str vectors copy their elements).
            let (p, l, elem) = vec_arg(arg, c, mdl, sigs);
            let dp = heap_copy_vec(&p, &l, elem, c, mdl);
            (dp, l.clone(), l)
        }
        HExpr::Call(..) => {
            // A call returning a vector: the %VecI64 struct comes back by
            // value; extract the header fields. The data pointer stays
            // ptr-typed — consumers ptrtoint it themselves when storing a
            // slot (an i64 here made the ctor's memcpy source ill-typed).
            let st = call_user_node(arg, c, mdl, sigs);
            let p = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 0", p, st));
            let l = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 1", l, st));
            let cap = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 2", cap, st));
            (p, l, cap)
        }
        other => {
            // Control-flow / clone / conj values in field position flow
            // through the general vector-value path; their header has no
            // spare capacity word, so cap = len.
            let (p, l, _) = emit_vec_pair(other, c, mdl, sigs);
            (p, l.clone(), l)
        }
    }
}

/// The deep-clone helper emitted for every boxed ADT: switch on the tag,
/// copy that variant's fields (boxed children recursively cloned), return
/// the fresh owning box.
pub(crate) fn emit_adt_clone_fn(adts: &[AdtDef], boxed: &[bool], i: u32) -> String {
    let def = &adts[i as usize];
    let name = format!("%{}", def.name);
    let slots = cato_core::adt_total_slots(adts, boxed, i);
    let mut out = String::new();
    out.push_str(&format!("define ptr @clone__{}(ptr %src) {{\n", def.name));
    out.push_str("entry:\n");
    out.push_str(&format!(
        "  %box = call ptr @malloc(i64 {})\n",
        8 * (1 + slots)
    ));
    out.push_str("  %tag = load i64, ptr %src\n");
    out.push_str("  store i64 %tag, ptr %box\n");
    let cases: Vec<String> = def
        .variants
        .iter()
        .enumerate()
        .map(|(vi, (vn, _))| format!("    i64 {}, label %v{}", vi, vn))
        .collect();
    let default = if def.variants.is_empty() {
        "merge".to_string()
    } else {
        "bad".to_string()
    };
    out.push_str(&format!(
        "  switch i64 %tag, label %{} [\n{}\n  ]\n",
        default,
        cases.join("\n")
    ));
    if !def.variants.is_empty() {
        out.push_str("bad:\n  unreachable\n\n");
    }
    let mut serial = 0usize;
    for (vn, fields) in def.variants.iter() {
        out.push_str(&format!("v{}:\n", vn));
        let mut at = 0usize;
        for f in fields {
            let count = cato_core::field_slot_count(f, boxed, adts);
            copy_field(&mut out, f, at, &name, adts, boxed, serial);
            serial += 1;
            at += count;
        }
        out.push_str("  br label %merge\n\n");
    }
    out.push_str("merge:\n  ret ptr %box\n}\n");
    let _ = i;
    out
}

/// Field copy inside `@clone__`: loads from %src at `off`, stores to %box.
fn copy_field(
    out: &mut String,
    f: &Ty,
    off: usize,
    name: &str,
    adts: &[AdtDef],
    boxed: &[bool],
    serial: usize,
) {
    // Locals are function-scoped in LLVM and variants repeat offsets, so
    // every field's temps carry a unique serial suffix.
    let u = format!("{off}x{serial}");
    let u = u.as_str();
    match f {
        Ty::Str => {
            // Owning copy of the bytes.
            out.push_str(&format!(
                "  %sp{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
            ));
            out.push_str(&format!("  %sl{u} = load i64, ptr %sp{u}\n"));
            out.push_str(&format!("  %sptr{u} = inttoptr i64 %sl{u} to ptr\n"));
            out.push_str(&format!("  %sdup{u} = call ptr @strdup(ptr %sptr{u})\n"));
            out.push_str(&format!(
                "  %dp{u} = getelementptr inbounds {name}, ptr %box, i32 0, i32 1, i32 {off}\n"
            ));
            out.push_str(&format!("  %ni{u} = ptrtoint ptr %sdup{u} to i64\n"));
            out.push_str(&format!("  store i64 %ni{u}, ptr %dp{u}\n"));
        }
        Ty::Vec(elem) => {
            // A vector field owns its element buffer, so the copy needs
            // its own: str elements strdup via cato_str_vec_copy, numerics
            // memcpy into a fresh buffer of the same capacity.
            out.push_str(&format!(
                "  %vp{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
            ));
            for k in 0..3usize {
                out.push_str(&format!(
                    "  %vf{u}k{k} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {}\n",
                    off + k
                ));
                out.push_str(&format!("  %vl{u}k{k} = load i64, ptr %vf{u}k{k}\n"));
            }
            out.push_str(&format!(
                "  %vptr{u} = inttoptr i64 %vl{u}k0 to ptr\n"
            ));
            if *elem == Elem::Str {
                out.push_str(&format!(
                    "  %vcp{u} = call ptr @cato_str_vec_copy(ptr %vptr{u}, i64 %vl{u}k1, i64 %vl{u}k2)\n"
                ));
            } else {
                out.push_str(&format!(
                    "  %vnb{u} = mul i64 %vl{u}k2, {}\n",
                    elem.bytes()
                ));
                out.push_str(&format!(
                    "  %vcp{u} = call ptr @malloc(i64 %vnb{u})\n"
                ));
                out.push_str(&format!(
                    "  %vcb{u} = mul i64 %vl{u}k1, {}\n",
                    elem.bytes()
                ));
                out.push_str(&format!(
                    "  call void @llvm.memcpy.p0.p0.i64(ptr %vcp{u}, ptr %vptr{u}, i64 %vcb{u}, i1 false, i1 false)\n"
                ));
            }
            for k in 0..3usize {
                out.push_str(&format!(
                    "  %vd{u}k{k} = getelementptr inbounds {name}, ptr %box, i32 0, i32 1, i32 {}\n",
                    off + k
                ));
                if k == 0 {
                    out.push_str(&format!(
                        "  %vni{u}k{k} = ptrtoint ptr %vcp{u} to i64\n"
                    ));
                    out.push_str(&format!(
                        "  store i64 %vni{u}k{k}, ptr %vd{u}k{k}\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "  store i64 %vl{u}k{k}, ptr %vd{u}k{k}\n"
                    ));
                }
            }
        }
        Ty::Adt(j)
            if boxed.get(*j as usize).copied().unwrap_or(false) =>
        {
            // Owning boxed child: clone recursively.
            let child = &adts[*j as usize].name;
            out.push_str(&format!(
                "  %sp{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
            ));
            out.push_str(&format!("  %sl{u} = load i64, ptr %sp{u}\n"));
            out.push_str(&format!("  %sc{u} = inttoptr i64 %sl{u} to ptr\n"));
            out.push_str(&format!(
                "  %nc{u} = call ptr @clone__{child}(ptr %sc{u})\n"
            ));
            out.push_str(&format!(
                "  %dp{u} = getelementptr inbounds {name}, ptr %box, i32 0, i32 1, i32 {off}\n"
            ));
            out.push_str(&format!("  %ni{u} = ptrtoint ptr %nc{u} to i64\n"));
            out.push_str(&format!("  store i64 %ni{u}, ptr %dp{u}\n"));
        }
        _ => {
            // Plain i64-slot copy (scalars, bools, strs, vec headers, and
            // inline ADT children copied by value need word-wise copies;
            // for inline children that is exactly their struct layout).
            let count = cato_core::field_slot_count(f, boxed, adts);
            for k in 0..count {
                let uk = format!("{u}k{k}");
                let uk = uk.as_str();
                out.push_str(&format!(
                    "  %sp{uk} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {}\n",
                    off + k
                ));
                out.push_str(&format!("  %sl{uk} = load i64, ptr %sp{uk}\n"));
                out.push_str(&format!(
                    "  %dp{uk} = getelementptr inbounds {name}, ptr %box, i32 0, i32 1, i32 {}\n",
                    off + k
                ));
                out.push_str(&format!("  store i64 %sl{uk}, ptr %dp{uk}\n"));
            }
        }
    }
}

/// The drop-glue helper emitted for every owning ADT: free the heap its
/// fields own (string bytes, vector buffers, child boxes via their own
/// glue), then — for boxed ADTs — the box itself. Inline owning children
/// are freed in place through their own glue at the embedded offset.
pub(crate) fn emit_adt_drop_fn(adts: &[AdtDef], owns: &[bool], boxed: &[bool], i: u32) -> String {
    let def = &adts[i as usize];
    let name = format!("%{}", def.name);
    let mut out = String::new();
    out.push_str(&format!("define void @drop__{}(ptr %src) {{\n", def.name));
    out.push_str("entry:\n");
    if boxed[i as usize] {
        // A nulled slot (an arm-exit free skipping a transferred child, or
        // a child that was never set) is simply not ours to free.
        out.push_str("  %nz = icmp ne ptr %src, null\n");
        out.push_str("  br i1 %nz, label %live, label %nul\n");
        out.push_str("nul:\n  ret void\n\n");
        out.push_str("live:\n");
        out.push_str("  %tag = load i64, ptr %src\n");
        let cases: Vec<String> = def
            .variants
            .iter()
            .enumerate()
            .map(|(vi, (vn, _))| format!("    i64 {}, label %v{}", vi, vn))
            .collect();
        let default = if def.variants.is_empty() {
            "freebox".to_string()
        } else {
            "bad".to_string()
        };
        out.push_str(&format!(
            "  switch i64 %tag, label %{} [\n{}\n  ]\n",
            default,
            cases.join("\n")
        ));
        if !def.variants.is_empty() {
            out.push_str("bad:\n  unreachable\n\n");
        }
        let mut serial = 0usize;
        for (vn, fields) in def.variants.iter() {
            out.push_str(&format!("v{}:\n", vn));
            let mut at = 0usize;
            for f in fields {
                free_field(&mut out, f, at, &name, adts, owns, boxed, serial);
                serial += 1;
                at += cato_core::field_slot_count(f, boxed, adts);
            }
            out.push_str("  br label %freebox\n\n");
        }
        out.push_str("freebox:\n  call void @free(ptr %src)\n  ret void\n}\n");
    } else {
        // Inline owning ADT: free fields in place; the storage itself is
        // the caller's slot (or an embedded region) and is not freed here.
        // The variant is unknown at compile time, so switch on the tag.
        out.push_str("  %tag = load i64, ptr %src\n");
        let cases: Vec<String> = def
            .variants
            .iter()
            .enumerate()
            .map(|(vi, (vn, _))| format!("    i64 {}, label %v{}", vi, vn))
            .collect();
        let default = if def.variants.is_empty() {
            "done".to_string()
        } else {
            "bad".to_string()
        };
        out.push_str(&format!(
            "  switch i64 %tag, label %{} [\n{}\n  ]\n",
            default,
            cases.join("\n")
        ));
        if !def.variants.is_empty() {
            out.push_str("bad:\n  unreachable\n\n");
        }
        let mut serial = 1000000usize;
        for (vn, fields) in def.variants.iter() {
            out.push_str(&format!("v{}:\n", vn));
            let mut at = 0usize;
            for f in fields {
                free_field(&mut out, f, at, &name, adts, owns, boxed, serial);
                serial += 1;
                at += cato_core::field_slot_count(f, boxed, adts);
            }
            out.push_str("  br label %done\n\n");
        }
        out.push_str("done:\n  ret void\n}\n");
    }
    out
}

/// Field free inside `@drop__`: `nofree_box` marks the inline-variant where
/// the storage itself is not freed.
fn free_field(
    out: &mut String,
    f: &Ty,
    off: usize,
    name: &str,
    adts: &[AdtDef],
    owns: &[bool],
    boxed: &[bool],
    serial: usize,
) {
    // Locals are function-scoped and variants repeat offsets: unique
    // serial suffix, same discipline as `copy_field`.
    let u = format!("{off}x{serial}");
    let u = u.as_str();
    match f {
        Ty::Str => {
            out.push_str(&format!(
                "  %fp{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
            ));
            out.push_str(&format!("  %fl{u} = load i64, ptr %fp{u}\n"));
            out.push_str(&format!("  %fv{u} = inttoptr i64 %fl{u} to ptr\n"));
            out.push_str(&format!("  call void @free(ptr %fv{u})\n"));
        }
        Ty::Vec(elem) => {
            out.push_str(&format!(
                "  %vp{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
            ));
            out.push_str(&format!("  %vl{u} = load i64, ptr %vp{u}\n"));
            out.push_str(&format!("  %vv{u} = inttoptr i64 %vl{u} to ptr\n"));
            if *elem == Elem::Str {
                // The array owns a heap copy of every element string.
                out.push_str(&format!(
                    "  %vn{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {}\n",
                    off + 1
                ));
                out.push_str(&format!("  %vc{u} = load i64, ptr %vn{u}\n"));
                out.push_str(&format!(
                    "  call void @cato_str_vec_free(ptr %vv{u}, i64 %vc{u})\n"
                ));
            } else {
                out.push_str(&format!("  call void @free(ptr %vv{u})\n"));
            }
        }
        Ty::Adt(j) if owns.get(*j as usize).copied().unwrap_or(false) => {
            if boxed.get(*j as usize).copied().unwrap_or(false) {
                // Owning boxed child: load the pointer, drop it.
                out.push_str(&format!(
                    "  %cp{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
                ));
                out.push_str(&format!("  %cl{u} = load i64, ptr %cp{u}\n"));
                out.push_str(&format!("  %cv{u} = inttoptr i64 %cl{u} to ptr\n"));
                out.push_str(&format!(
                    "  call void @drop__{}(ptr %cv{u})\n",
                    adts[*j as usize].name
                ));
            } else {
                // Owning inline child: free in place at the embedded offset.
                out.push_str(&format!(
                    "  %ep{u} = getelementptr inbounds {name}, ptr %src, i32 0, i32 1, i32 {off}\n"
                ));
                out.push_str(&format!(
                    "  call void @drop__{}(ptr %ep{u})\n",
                    adts[*j as usize].name
                ));
            }
        }
        _ => {
            let _ = name;
        }
    }
}
