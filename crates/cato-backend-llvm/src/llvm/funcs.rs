use super::*;

pub(crate) fn emit_plain(defn: &Defn, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    let mut params = Vec::new();
    for (pname, pty) in &defn.params {
        match pty {
            Ty::Vec(e) => {
                let d = format!("%{}_data", mangle(pname));
                let l = format!("%{}_len", mangle(pname));
                c.vecs.insert(pname.clone(), (d.clone(), l.clone(), *e));
                params.push(format!("ptr noalias nocapture readonly {}", d));
                params.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::F64));
                params.push(format!("double {}", r));
            }
            t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), *t));
                params.push(format!("{} {}", Mdl::ll_scalar(*t), r));
            }
            Ty::F32 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::F32));
                params.push(format!("float {}", r));
            }
            Ty::Bool => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Bool));
                params.push(format!("i1 {}", r));
            }
            Ty::Str => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Str));
                params.push(format!("ptr {}", r));
            }
            Ty::Map => {
                // (ptr, len, cap) triple copied into an entry slot: map ops
                // read — and owned in-place growth rewrites — the local
                // slot only, never the caller's frame.
                let d = format!("%{}_data", mangle(pname));
                let l = format!("%{}_len", mangle(pname));
                let cp = format!("%{}_cap", mangle(pname));
                params.push(format!("ptr noalias nocapture readonly {}", d));
                params.push(format!("i64 {}", l));
                params.push(format!("i64 {}", cp));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %VecI64, align 8", slot));
                let pf = c.temp();
                c.prelude.push(format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
                    pf, slot
                ));
                c.prelude
                    .push(format!("  store ptr {}, ptr {}, align 8", d, pf));
                let lf = c.temp();
                c.prelude.push(format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
                    lf, slot
                ));
                c.prelude
                    .push(format!("  store i64 {}, ptr {}, align 8", l, lf));
                let cf = c.temp();
                c.prelude.push(format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
                    cf, slot
                ));
                c.prelude
                    .push(format!("  store i64 {}, ptr {}, align 8", cp, cf));
                c.growm.insert(pname.clone(), (slot, *pty));
            }
            Ty::Set => {
                // (ptr, len) pair copied into an entry slot, like maps
                // minus the representation tag.
                let d = format!("%{}_data", mangle(pname));
                let l = format!("%{}_len", mangle(pname));
                params.push(format!("ptr noalias nocapture readonly {}", d));
                params.push(format!("i64 {}", l));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %VecI64, align 8", slot));
                let pf = c.temp();
                c.prelude.push(format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
                    pf, slot
                ));
                c.prelude
                    .push(format!("  store ptr {}, ptr {}, align 8", d, pf));
                let lf = c.temp();
                c.prelude.push(format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
                    lf, slot
                ));
                c.prelude
                    .push(format!("  store i64 {}, ptr {}, align 8", l, lf));
                c.growm.insert(pname.clone(), (slot, *pty));
            }
            Ty::Fn(_) => {
                // Closure params arrive by value; copy into an entry slot.
                let r = format!("%{}", mangle(pname));
                params.push(format!("%Closure {}", r));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %Closure, align 8", slot));
                c.prelude
                    .push(format!("  store %Closure {}, ptr {}, align 8", r, slot));
                c.scalar.insert(pname.clone(), (slot, *pty));
            }
            Ty::Param(_) => panic!("codegen: unresolved type parameter in params"),
            Ty::Adt(i) if mdl.adt_boxed(*i) => {
                // Boxed ADT: the value IS the owning heap pointer; moves
                // transfer it, no copy at the boundary.
                let r = format!("%{}", mangle(pname));
                params.push(format!("ptr {}", r));
                c.scalar.insert(pname.clone(), (r, *pty));
            }
            Ty::Adt(i) => {
                // By-value boundary: the incoming struct is copied into an
                // entry slot; all internal uses go through the ptr.
                let r = format!("%{}", mangle(pname));
                params.push(format!("{} {}", mdl.adt_name(*i), r));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(*i)));
                c.prelude.push(format!(
                    "  store {} {}, ptr {}, align 8",
                    mdl.adt_name(*i),
                    r,
                    slot
                ));
                c.scalar.insert(pname.clone(), (slot, *pty));
            }
        }
    }

    c.line("entry:");
    c.open("entry");
    if defn.name == "main" {
        // S3: argv plumbing — `(args)` reads through the runtime.
        mdl.ensure_decl("declare void @cato_args_init(i32, ptr)");
        c.line("  call void @cato_args_init(i32 %argc, ptr %argv)");
        for form in seq_forms(&defn.body) {
            emit_stmt(&form, &mut c, mdl, sigs);
        }
    } else if let Ty::Vec(e) = defn.ret {
        // Vector return: the %VecI64 struct goes back by value.
        let (p, l, _) = emit_vec_pair(&defn.body, &mut c, mdl, sigs);
        let _ = e;
        let s0 = c.temp();
        c.line(&format!(
            "  {} = insertvalue %VecI64 undef, ptr {}, 0",
            s0, p
        ));
        let s1 = c.temp();
        c.line(&format!(
            "  {} = insertvalue %VecI64 {}, i64 {}, 1",
            s1, s0, l
        ));
        let _ = e;
        c.line(&format!("  ret %VecI64 {}", s1));
    } else if matches!(defn.ret, Ty::Map | Ty::Set) {
        // Map/set return: the header goes back by value in the same
        // %VecI64 shape — maps with the representation tag in field 2,
        // sets with an unused cap of 0.
        let header = emit_collection_ret(&defn.body, defn.ret, &mut c, mdl, sigs);
        c.line(&format!("  ret %VecI64 {}", header));
    } else {
        let val = emit_for_ret(&defn.body, &mut c, mdl, sigs, defn.ret);
        if let Ty::Adt(i) = defn.ret {
            if mdl.adt_boxed(i) {
                // Boxed: the pointer itself is returned; ownership moves.
                c.line(&format!("  ret ptr {}", val));
            } else {
                // Copy the result slot out by value; a returned ptr would
                // dangle.
                let lv = c.temp();
                c.line(&format!(
                    "  {} = load {}, ptr {}, align 8",
                    lv,
                    mdl.adt_name(i),
                    val
                ));
                c.line(&format!("  ret {} {}", mdl.adt_name(i), lv));
            }
        } else {
            let ret_ll = if defn.ret == Ty::Bool {
                "i1"
            } else if defn.ret == Ty::Str {
                "ptr"
            } else if defn.ret.is_int() || defn.ret.is_float() {
                Mdl::ll_scalar(defn.ret)
            } else {
                "i64"
            };
            c.line(&format!("  ret {} {}", ret_ll, val));
        }
    }

    let ret_ll = if defn.name == "main" {
        "i32".to_string()
    } else if defn.ret == Ty::Bool {
        "i1".to_string()
    } else if defn.ret == Ty::F64 {
        "double".to_string()
    } else if defn.ret == Ty::Str {
        "ptr".to_string()
    } else if defn.ret == Ty::F32 {
        "float".to_string()
    } else if let Ty::Adt(i) = defn.ret {
        if mdl.adt_boxed(i) {
            "ptr".to_string()
        } else {
            mdl.adt_name(i)
        }
    } else if matches!(defn.ret, Ty::Vec(_) | Ty::Map | Ty::Set) {
        "%VecI64".to_string()
    } else if defn.ret.is_int() || defn.ret.is_float() {
        Mdl::ll_scalar(defn.ret).to_string()
    } else {
        "i64".to_string()
    };
    let mut text = format!("define {} @{}(", ret_ll, mangle(&defn.name));
    text.push_str(&params.join(", "));
    if defn.name == "main" {
        if !params.is_empty() {
            text.push_str(", ");
        }
        text.push_str("i32 %argc, ptr %argv");
    }
    text.push_str(") {\n");
    text.push_str(&FnCtx::with_prelude(c.lines, &c.prelude).join("\n"));
    if defn.name == "main" {
        text.push('\n');
        text.push_str("  ret i32 0\n");
    }
    text.push_str("}\n");
    text
}

/// FFI declaration: a plain LLVM `declare` — direct C calls, no marshaling.
pub(crate) fn emit_extern_decl(defn: &Defn, _mdl: &mut Mdl) -> String {
    let mut params = Vec::new();
    for (_, pty) in &defn.params {
        match pty {
            Ty::Vec(e) => {
                params.push(format!("ptr noalias nocapture readonly %data",));
                params.push("i64 %len".to_string());
                let _ = e;
            }
            Ty::F64 => params.push("double".to_string()),
            t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => params.push(Mdl::ll_scalar(*t).to_string()),
            Ty::F32 => params.push("float".to_string()),
            Ty::Bool => params.push("i1".to_string()),
            Ty::Str => params.push("ptr".to_string()),
            Ty::Map => panic!("codegen: map params not supported"),
            Ty::Set => panic!("codegen: set params not supported"),
            Ty::Fn(_) => panic!("codegen: closure params not supported on externs"),
            Ty::Adt(_) | Ty::Param(_) => {
                panic!("codegen: extern params must be scalars, strings or vectors")
            }
        }
    }
    let ret_ll = match defn.ret {
        Ty::I64 => "i64",
        Ty::F64 => "double",
        Ty::Bool => "i1",
        Ty::Str => "ptr",
        _ => panic!("codegen: extern return must be i64, f64, bool or str"),
    };
    let line = format!(
        "declare {} @{}({})",
        ret_ll,
        mangle(&defn.name),
        params.join(", ")
    );
    line
}

pub(crate) fn seq_forms(b: &HExpr) -> Vec<HExpr> {
    match b {
        HExpr::Seq(vs) => vs.clone(),
        other => vec![other.clone()],
    }
}

pub(crate) fn emit_for_ret(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>, ret: Ty) -> String {
    match ret {
        Ty::F64 | Ty::F32 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
            let _ = t;
            emit_int(e, c, mdl, sigs)
        }
        Ty::Adt(_) => emit_adt(e, c, mdl, sigs),
        Ty::Str => emit_str_val(e, c, mdl, sigs),
        Ty::Vec(_) => panic!("codegen: cannot return a vector"),
        Ty::Map => panic!("codegen: cannot return a map (maps are local values)"),
        Ty::Set => panic!("codegen: cannot return a set (sets are local values)"),
        Ty::Fn(_) => panic!("codegen: cannot return a closure yet"),
        Ty::Param(_) => panic!("codegen: unresolved type parameter"),
    }
}

pub(crate) fn emit_stmt(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
    match e {
        HExpr::Call(name, args) if name == "print-f64" => {
            let v = emit_float(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_f64(double {})", v));
        }
        HExpr::Call(name, args) if name == "print-i64" => {
            let v = emit_int(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_i64(i64 {})", v));
        }
        HExpr::Call(name, args) if name == "print-str" => {
            let v = emit_str_val(&args[0], c, mdl, sigs);
            mdl.ensure_decl("declare void @cato_print_str_raw(ptr)");
            c.line(&format!("  call void @cato_print_str_raw(ptr {})", v));
        }
        HExpr::Spit { path, data } => {
            let pv = emit_str_val(path, c, mdl, sigs);
            let dv = emit_str_val(data, c, mdl, sigs);
            mdl.ensure_decl("declare void @cato_spit(ptr, ptr)");
            c.line(&format!("  call void @cato_spit(ptr {}, ptr {})", pv, dv));
        }
        HExpr::Exit(c2) => {
            let v = emit_int(c2, c, mdl, sigs);
            mdl.ensure_decl("declare void @cato_exit(i64)");
            c.line(&format!("  call void @cato_exit(i64 {})", v));
        }
        HExpr::DropStmt(inner) => {
            // An explicit `drop` releases the binding's heap now (the
            // ownership planner already suppressed its scope-exit free,
            // so this is the one destruction point).
            if let HExpr::Var(v) = &**inner {
                free_binding_storage(v, c, mdl);
            }
        }
        _ => {
            emit_for_ret(e, c, mdl, sigs, Ty::I64);
        }
    }
}

// ---------------------------------------------------------------------------
// Expression emission
// ---------------------------------------------------------------------------

pub(crate) fn emit_typed(
    e: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match ty {
        Ty::F64 | Ty::F32 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
            let _ = t;
            emit_int(e, c, mdl, sigs)
        }
        Ty::Adt(_) => emit_adt(e, c, mdl, sigs),
        Ty::Str => emit_str_val(e, c, mdl, sigs),
        Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Fn(_) => {
            panic!("codegen: control flow cannot produce an aggregate")
        }
        Ty::Param(_) => panic!("codegen: unresolved type parameter"),
    }
}

/// Build the by-value `%VecI64` header for a map/set return: maps carry
/// (ptr, len, cap) — cap is the flat/HAMT representation tag — and sets
/// (ptr, len) with an unused cap of 0, so both share the vector struct.
pub(crate) fn emit_collection_ret(
    e: &HExpr,
    ret: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let (p, l, cap) = if ret == Ty::Map {
        map_val(e, c, mdl, sigs)
    } else {
        let (p, l) = set_val(e, c, mdl, sigs);
        (p, l, "0".to_string())
    };
    let s0 = c.temp();
    c.line(&format!(
        "  {} = insertvalue %VecI64 undef, ptr {}, 0",
        s0, p
    ));
    let s1 = c.temp();
    c.line(&format!("  {} = insertvalue %VecI64 {}, i64 {}, 1", s1, s0, l));
    let s2 = c.temp();
    c.line(&format!("  {} = insertvalue %VecI64 {}, i64 {}, 2", s2, s1, cap));
    s2
}

pub(crate) fn emit_vec_pair(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String, Elem) {
    match e {
        HExpr::Var(v) => {
            if c.grow.contains_key(v) {
                grow_val(c, v)
            } else {
                vec_arg(e, c, mdl, sigs)
            }
        }
        HExpr::VecLit(_) => vec_arg(e, c, mdl, sigs),
        HExpr::Call(..) => emit_vec_call(e, c, mdl, sigs),
        HExpr::Split { s, sep } => {
            let (arr, l) = emit_split(s, sep, c, mdl, sigs);
            (arr, l, Elem::Str)
        }
        HExpr::Conj { v, x, elem, in_place } => {
            // A conj in value position: build through the same slot
            // machinery as a let-bound conj, then read the live header.
            let slot = emit_conj(v, x, *elem, *in_place, c, mdl, sigs);
            read_grow_slot(&slot, *elem, c)
        }
        HExpr::Clone(inner) => {
            // Deep copy of a vector value: str elements strdup through
            // cato_str_vec_copy (each vector owns its element strings),
            // numerics memcpy into a fresh exact-size buffer.
            let (p, l, elem) = emit_vec_pair(inner, c, mdl, sigs);
            let np = if elem == Elem::Str {
                mdl.ensure_decl("declare ptr @cato_str_vec_copy(ptr, i64, i64)");
                let d = c.temp();
                c.line(&format!(
                    "  {} = call ptr @cato_str_vec_copy(ptr {}, i64 {}, i64 {})",
                    d, p, l, l
                ));
                d
            } else {
                let nbytes = c.temp();
                c.line(&format!("  {} = mul i64 {}, 8", nbytes, l));
                let d = emit_malloc(c, mdl, &nbytes);
                mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
                c.line(&format!(
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
                    d, p, nbytes
                ));
                d
            };
            let slot = c.temp();
            c.prelude.push(format!("  {} = alloca %VecI64, align 8", slot));
            for (k, v) in [(0, np.clone()), (1, l.clone()), (2, l)] {
                let f = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 {}",
                    f, slot, k
                ));
                if k == 0 {
                    c.line(&format!("  store ptr {}, ptr {}, align 8", v, f));
                } else {
                    c.line(&format!("  store i64 {}, ptr {}, align 8", v, f));
                }
            }
            read_grow_slot(&slot, elem, c)
        }
        HExpr::If(cond, then, els, ty) => {
            let elem = vec_elem_of(*ty);
            let lc = emit_bool(cond, c, mdl, sigs);
            let bt = c.block_name();
            let be = c.block_name();
            let bm = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", lc, bt, be));
            let lbl = c.open(&bt);
            c.line(&format!("{}:", lbl));
            let (tp, tl, _) = emit_vec_pair(then, c, mdl, sigs);
            let tbr = c.cur.clone();
            c.line(&format!("  br label %{}", bm));
            let lbl = c.open(&be);
            c.line(&format!("{}:", lbl));
            let (ep, el, _) = emit_vec_pair(els, c, mdl, sigs);
            let ebr = c.cur.clone();
            c.line(&format!("  br label %{}", bm));
            let lbl = c.open(&bm);
            c.line(&format!("{}:", lbl));
            phi_vec_pair(&tp, &tl, &tbr, &ep, &el, &ebr, elem, c)
        }
        HExpr::Match { scrut, arms, ty } => {
            let elem = vec_elem_of(*ty);
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
            // Default: HIR guarantees exhaustiveness.
            let lbl = c.open(&fail);
            c.line(&format!("{}:", lbl));
            c.line("  unreachable");
            let mut ps: Vec<String> = Vec::new();
            let mut ls: Vec<String> = Vec::new();
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
                let (p, l2, _) = emit_vec_pair(&arm.body, c, mdl, sigs);
                ps.push(p);
                ls.push(l2);
                preds.push(c.cur.clone());
                c.line(&format!("  br label %{}", merged));
            }
            let lbl = c.open(&merged);
            c.line(&format!("{}:", lbl));
            let p0 = ps.first().cloned().unwrap_or_else(|| "null".into());
            let l0 = ls.first().cloned().unwrap_or_else(|| "0".into());
            let m0 = phi_pairs("ptr", &ps, &preds, &p0, c);
            let m1 = phi_pairs("i64", &ls, &preds, &l0, c);
            (m0, m1, elem)
        }
        HExpr::Let(binds, body, frees) => {
            emit_let(binds, c, mdl, sigs);
            let v = emit_vec_pair(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_vec_pair(vs.last().unwrap(), c, mdl, sigs)
        }
        other => panic!("codegen: unsupported vector value {:?}", other),
    }
}

/// The element type of a vector-typed expression annotation.
fn vec_elem_of(ty: Ty) -> Elem {
    match ty {
        Ty::Vec(e) => e,
        other => panic!("codegen: vector value annotated {:?}", other),
    }
}

/// Read a growable slot's live (ptr, len).
fn read_grow_slot(slot: &str, elem: Elem, c: &mut FnCtx) -> (String, String, Elem) {
    let pf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
        pf, slot
    ));
    let p = c.temp();
    c.line(&format!("  {} = load ptr, ptr {}, align 8", p, pf));
    let lf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
        lf, slot
    ));
    let l = c.temp();
    c.line(&format!("  {} = load i64, ptr {}, align 8", l, lf));
    (p, l, elem)
}

/// Join one (ptr, len) pair arriving from two predecessor blocks.
fn phi_vec_pair(
    tp: &str,
    tl: &str,
    tbr: &str,
    ep: &str,
    el: &str,
    ebr: &str,
    elem: Elem,
    c: &mut FnCtx,
) -> (String, String, Elem) {
    let mp = c.temp();
    c.line(&format!(
        "  {} = phi ptr [ {}, %{} ], [ {}, %{} ]",
        mp, tp, tbr, ep, ebr
    ));
    let ml = c.temp();
    c.line(&format!(
        "  {} = phi i64 [ {}, %{} ], [ {}, %{} ]",
        ml, tl, tbr, el, ebr
    ));
    (mp, ml, elem)
}

/// Phi over N values of `llty` from `preds`; `dflt`/fallback when empty.
fn phi_pairs(llty: &str, vals: &[String], preds: &[String], dflt: &str, c: &mut FnCtx) -> String {
    let res = c.temp();
    if vals.is_empty() {
        c.line(&format!("  {} = add {} 0, 0", res, llty));
        return res;
    }
    let pairs: Vec<String> = vals
        .iter()
        .zip(preds.iter())
        .map(|(v, p)| format!("[ {}, %{} ]", v, p))
        .collect();
    c.line(&format!("  {} = phi {} {}", res, llty, pairs.join(", ")));
    let _ = dflt;
    res
}

/// Static width of an i64/i32 expression (for cast selection).
pub(crate) fn static_int_width(e: &HExpr, c: &FnCtx, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> Ty {
    match e {
        HExpr::ConstI64(_) => Ty::I64,
        HExpr::ConstB(_) => Ty::Bool,
        HExpr::ConstF64(_) => Ty::F64,
        HExpr::ICast(_, _, t) => *t,
        HExpr::BinW(.., t) => *t,
        HExpr::BinF(..) => Ty::F32,
        HExpr::Abs(_, t) | HExpr::Min(_, _, t) | HExpr::Max(_, _, t) => *t,
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(_, t)| *t)
            .unwrap_or(Ty::I64),
        HExpr::Call(name, _) => sigs
            .get(name)
            .map(|(r, _)| *r)
            .unwrap_or(Ty::I64),
        _ => Ty::I64,
    }
}

