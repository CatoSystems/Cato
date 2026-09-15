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
            emit_let(binds, c, mdl, sigs);
            let v = emit_adt(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
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
            let def = mdl.adts[ai as usize].clone();
            let fields = def.variants[vi as usize].1.clone();
            let offsets = &mdl.adt_field_offsets(ai)[vi as usize];
            for (k, (binder, fty)) in arm.binders.iter().zip(fields.iter()).enumerate() {
                emit_field_load(binder, fty, offsets[k], ai, name.clone(), scrut_ptr.clone(), c, mdl);
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
            let (p, l, cap) = emit_vec_header(arg, c, mdl, sigs);
            for (k, v) in [p, l, cap].into_iter().enumerate() {
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
            let _ = elem;
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
                    c.prelude
                        .push(format!("  store ptr {}, ptr {}, align 8", t_p, t_dst));
                } else {
                    c.prelude
                        .push(format!("  store i64 {}, ptr {}, align 8", t_l, t_dst));
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
                let (p, l, _) = c
                    .vecs
                    .get(v)
                    .cloned()
                    .unwrap_or_else(|| panic!("codegen: {} is not a vector binding", v));
                (p, l.clone(), l)
            }
        }
        HExpr::VecLit(_) => {
            let (p, l, _) = vec_arg(arg, c, mdl);
            (p, l.clone(), l)
        }
        HExpr::Call(..) => {
            // A call returning a vector: the %VecI64 struct comes back by
            // value; extract the header fields.
            let st = call_user_node(arg, c, mdl, sigs);
            let p = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 0", p, st));
            let pp = c.temp();
            c.line(&format!("  {} = ptrtoint ptr {} to i64", pp, p));
            let l = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 1", l, st));
            let cap = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 2", cap, st));
            (pp, l, cap)
        }
        other => panic!("codegen: unsupported vector field argument {:?}", other),
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
    for (vn, fields) in def.variants.iter() {
        out.push_str(&format!("v{}:\n", vn));
        let mut at = 0usize;
        for f in fields {
            let count = cato_core::field_slot_count(f, boxed, adts);
            copy_field(&mut out, f, at, &name, adts, boxed);
            at += count;
        }
        out.push_str("  br label %merge\n\n");
    }
    out.push_str("merge:\n  ret ptr %box\n}\n");
    let _ = i;
    out
}

/// Field copy inside `@clone__`: loads from %src at `off`, stores to %box.
fn copy_field(out: &mut String, f: &Ty, off: usize, name: &str, adts: &[AdtDef], boxed: &[bool]) {
    match f {
        Ty::Str => {
            // Owning copy of the bytes.
            out.push_str(&format!(
                "  %sp{0} = getelementptr inbounds {2}, ptr %src, i32 0, i32 1, i32 {1}\n",
                off, off, name
            ));
            out.push_str(&format!("  %sl{0} = load i64, ptr %sp{0}\n", off));
            out.push_str(&format!("  %sptr{0} = inttoptr i64 %sl{0} to ptr\n", off));
            out.push_str(&format!("  %sdup{0} = call ptr @strdup(ptr %sptr{0})\n", off));
            out.push_str(&format!(
                "  %dp{0} = getelementptr inbounds {2}, ptr %box, i32 0, i32 1, i32 {1}\n",
                off, off, name
            ));
            out.push_str(&format!(
                "  %ni{0} = ptrtoint ptr %sdup{0} to i64\n",
                off
            ));
            out.push_str(&format!("  store i64 %ni{}, ptr %dp{}\n", off, off));
        }
        Ty::Adt(j)
            if boxed.get(*j as usize).copied().unwrap_or(false) =>
        {
            // Owning boxed child: clone recursively.
            let child = &adts[*j as usize].name;
            out.push_str(&format!(
                "  %sp{} = getelementptr inbounds {}, ptr %src, i32 0, i32 1, i32 {}\n",
                off, name, off
            ));
            out.push_str(&format!("  %sl{} = load i64, ptr %sp{}\n", off, off));
            out.push_str(&format!(
                "  %sc{} = inttoptr i64 %sl{} to ptr\n",
                off, off
            ));
            out.push_str(&format!(
                "  %nc{} = call ptr @clone__{}(ptr %sc{})\n",
                off, child, off
            ));
            out.push_str(&format!(
                "  %dp{0} = getelementptr inbounds {1}, ptr %box, i32 0, i32 1, i32 {0}\n",
                off, name
            ));
            out.push_str(&format!(
                "  %ni{0} = ptrtoint ptr %nc{0} to i64\n",
                off
            ));
            out.push_str(&format!("  store i64 %ni{}, ptr %dp{}\n", off, off));
        }
        _ => {
            // Plain i64-slot copy (scalars, bools, strs, vec headers, and
            // inline ADT children copied by value need word-wise copies;
            // for inline children that is exactly their struct layout).
            let count = cato_core::field_slot_count(f, boxed, adts);
            for k in 0..count {
                out.push_str(&format!(
                    "  %sp{0} = getelementptr inbounds {2}, ptr %src, i32 0, i32 1, i32 {1}\n",
                    off + k,
                    off + k,
                    name
                ));
                out.push_str(&format!("  %sl{0} = load i64, ptr %sp{0}\n", off + k));
                out.push_str(&format!(
                    "  %dp{0} = getelementptr inbounds {2}, ptr %box, i32 0, i32 1, i32 {1}\n",
                    off + k,
                    off + k,
                    name
                ));
                out.push_str(&format!("  store i64 %sl{0}, ptr %dp{0}\n", off + k));
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
        for (vn, fields) in def.variants.iter() {
            out.push_str(&format!("v{}:\n", vn));
            let mut at = 0usize;
            for f in fields {
                free_field(&mut out, f, at, &name, adts, owns, boxed);
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
        for (vn, fields) in def.variants.iter() {
            out.push_str(&format!("v{}:\n", vn));
            let mut at = 0usize;
            for f in fields {
                free_field(&mut out, f, at, &name, adts, owns, boxed);
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
) {
    match f {
        Ty::Str => {
            out.push_str(&format!(
                "  %fp{0} = getelementptr inbounds {2}, ptr %src, i32 0, i32 1, i32 {1}\n",
                off, off, name
            ));
            out.push_str(&format!("  %fl{0} = load i64, ptr %fp{0}\n", off));
            out.push_str(&format!("  %fv{0} = inttoptr i64 %fl{0} to ptr\n", off));
            out.push_str(&format!("  call void @free(ptr %fv{})\n", off));
        }
        Ty::Vec(_) => {
            out.push_str(&format!(
                "  %vp{0} = getelementptr inbounds {2}, ptr %src, i32 0, i32 1, i32 {1}\n",
                off, off, name
            ));
            out.push_str(&format!("  %vl{0} = load i64, ptr %vp{0}\n", off));
            out.push_str(&format!("  %vv{0} = inttoptr i64 %vl{0} to ptr\n", off));
            out.push_str(&format!("  call void @free(ptr %vv{})\n", off));
        }
        Ty::Adt(j) if owns.get(*j as usize).copied().unwrap_or(false) => {
            if boxed.get(*j as usize).copied().unwrap_or(false) {
                // Owning boxed child: load the pointer, drop it.
                out.push_str(&format!(
                    "  %cp{0} = getelementptr inbounds {2}, ptr %src, i32 0, i32 1, i32 {1}\n",
                    off, off, name
                ));
                out.push_str(&format!("  %cl{0} = load i64, ptr %cp{0}\n", off));
                out.push_str(&format!("  %cv{0} = inttoptr i64 %cl{0} to ptr\n", off));
                out.push_str(&format!(
                    "  call void @drop__{}(ptr %cv{})\n",
                    adts[*j as usize].name, off
                ));
            } else {
                // Owning inline child: free in place at the embedded offset.
                out.push_str(&format!(
                    "  %ep{0} = getelementptr inbounds {2}, ptr %src, i32 0, i32 1, i32 {1}\n",
                    off, off, name
                ));
                out.push_str(&format!(
                    "  call void @drop__{}(ptr %ep{})\n",
                    adts[*j as usize].name, off
                ));
            }
        }
        _ => {
            let _ = name;
        }
    }
}
