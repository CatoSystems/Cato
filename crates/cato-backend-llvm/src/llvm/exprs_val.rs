use super::*;

pub(crate) fn materialize_vec_lit(value: &HExpr, name: &str, c: &mut FnCtx, mdl: &mut Mdl) {
    let HExpr::VecLit(lit) = value else {
        panic!("codegen: can only materialize vector literal bindings")
    };
    let (label, len, elem) = mdl.mint_vec(lit);
    let t_v = c.temp();
    // The slot hoists to the entry prelude: a literal evaluated inside a
    // loop body would otherwise alloca once per iteration and walk the
    // stack (the same discipline as emit_conj's growable slots).
    c.prelude
        .push(format!("  {} = alloca %VecI64, align 8", t_v));
    c.line(&format!(
        "  store %VecI64 {{ ptr {}, i64 {}, i64 {} }}, ptr {}, align 8",
        label, len, len, t_v
    ));
    let t_lv = c.temp();
    c.line(&format!("  {} = load %VecI64, ptr {}, align 8", t_lv, t_v));
    let t_d = c.temp();
    let t_l = c.temp();
    c.line(&format!("  {} = extractvalue %VecI64 {}, 0", t_d, t_lv));
    c.line(&format!("  {} = extractvalue %VecI64 {}, 1", t_l, t_lv));
    c.vecs.insert(name.to_string(), (t_d, t_l, elem));
}

/// Load a growable binding's current (ptr, len) — reads the live header.

pub(crate) fn grow_val(c: &mut FnCtx, name: &str) -> (String, String, Elem) {
    let (slot, elem) = c
        .grow
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("codegen: {} is not a growable binding", name));
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

/// Emit a body expression in (ptr, len) vector-value position: growable
/// bindings load their live header; lets free-stand by emitting their
/// bindings first. Anything beyond the Var/VecLit fast paths flows through
/// the general `emit_vec_pair` (if/match/conj/clone/call/...).

pub(crate) fn vec_arg(
    expr: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String, Elem) {
    match expr {
        HExpr::Var(v) => {
            if c.grow.contains_key(v) {
                return grow_val(c, v);
            }
            c.vecs
                .get(v)
                .cloned()
                .unwrap_or_else(|| panic!("codegen: {} is not a vector binding", v))
        }
        HExpr::VecLit(lit) => {
            let (label, len, elem) = mdl.mint_vec(lit);
            let t_v = c.temp();
            // Hoisted to the entry prelude: loop-safe, like materialize_vec_lit.
            c.prelude
                .push(format!("  {} = alloca %VecI64, align 8", t_v));
            c.line(&format!(
                "  store %VecI64 {{ ptr {}, i64 {}, i64 {} }}, ptr {}, align 8",
                label, len, len, t_v
            ));
            let t_lv = c.temp();
            c.line(&format!("  {} = load %VecI64, ptr {}, align 8", t_lv, t_v));
            let t_d = c.temp();
            let t_l = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 0", t_d, t_lv));
            c.line(&format!("  {} = extractvalue %VecI64 {}, 1", t_l, t_lv));
            (t_d, t_l, elem)
        }
        other => emit_vec_pair(other, c, mdl, sigs),
    }
}

pub(crate) fn emit_str_val(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::StrLit(s) => {
            let label = mdl.mint_str(s);
            let n = s.len() + 1;
            let p = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds [{} x i8], ptr {}, i32 0, i32 0",
                p, n, label
            ));
            p
        }
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(r, _)| r.clone())
            .unwrap_or_else(|| panic!("codegen: unknown string var {}", v)),
        HExpr::Subs { s, start, end } => emit_subs(s, start, end, c, mdl, sigs),
        HExpr::Loop { .. } => emit_loop_node(e, c, mdl, sigs),
        HExpr::StrCat(args) => emit_strcat(args, c, mdl, sigs),
        HExpr::ToStr(inner, t) => {
            // Number/bool → runtime decimal text (malloc'd, owned).
            let v = match t {
                Ty::F64 => {
                    let f = emit_float(inner, c, mdl, sigs);
                    mdl.declare("declare ptr @cato_f64_to_str(double)".to_string());
                    let r = c.temp();
                    c.line(&format!(
                        "  {} = call ptr @cato_f64_to_str(double {})",
                        r, f
                    ));
                    r
                }
                Ty::Bool => {
                    let b = emit_bool(inner, c, mdl, sigs);
                    let z = c.temp();
                    c.line(&format!("  {} = zext i1 {} to i64", z, b));
                    mdl.declare("declare ptr @cato_num_to_str(i64)".to_string());
                    let r = c.temp();
                    c.line(&format!("  {} = call ptr @cato_num_to_str(i64 {})", r, z));
                    r
                }
                t if t.is_uint() => {
                    let i = emit_int(inner, c, mdl, sigs);
                    mdl.declare("declare ptr @cato_u64_to_str(i64)".to_string());
                    let r = c.temp();
                    c.line(&format!("  {} = call ptr @cato_u64_to_str(i64 {})", r, i));
                    r
                }
                _ => {
                    let i = emit_int(inner, c, mdl, sigs);
                    mdl.declare("declare ptr @cato_num_to_str(i64)".to_string());
                    let r = c.temp();
                    c.line(&format!("  {} = call ptr @cato_num_to_str(i64 {})", r, i));
                    r
                }
            };
            v
        }
        HExpr::Slurp(p) => {
            let pv = emit_str_val(p, c, mdl, sigs);
            mdl.declare("declare ptr @cato_slurp(ptr)".to_string());
            let r = c.temp();
            c.line(&format!("  {} = call ptr @cato_slurp(ptr {})", r, pv));
            r
        }
        HExpr::CharStr(inner) => {
            // A fresh 2-byte allocation: the character byte plus NUL. The
            // value is the low byte of the operand (i8 trunc semantics).
            let cv = emit_int(inner, c, mdl, sigs);
            let p = emit_malloc(c, mdl, "2");
            let b = c.temp();
            c.line(&format!("  {} = trunc i64 {} to i8", b, cv));
            c.line(&format!("  store i8 {}, ptr {}", b, p));
            let np = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds i8, ptr {}, i64 1",
                np, p
            ));
            c.line(&format!("  store i8 0, ptr {}", np));
            p
        }
        HExpr::If(cond, then, els, Ty::Str) => emit_if_node(cond, then, els, Ty::Str, c, mdl, sigs),
        HExpr::Match { ty: Ty::Str, .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Let(binds, body, frees, rebind) => {
            emit_let_scoped(binds, frees, rebind, c, mdl, sigs, |c, mdl| {
                emit_str_val(body, c, mdl, sigs)
            })
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_str_val(vs.last().unwrap(), c, mdl, sigs)
        }
        HExpr::Join { sep, xs } => emit_join(sep, xs, c, mdl, sigs),
        HExpr::Get {
            src,
            idx,
            default,
            ty: Ty::Str,
            ..
        } => {
            // Element load from a (Vector str): bounds-checked, null (or the
            // default string) out of range.
            let (d, l, _) = vec_arg(src, c, mdl, sigs);
            let iv = emit_int(idx, c, mdl, sigs);
            let inb = c.temp();
            c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, l));
            let safe = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, i64 {}, i64 0",
                safe, inb, iv
            ));
            let p = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds ptr, ptr {}, i64 {}",
                p, d, safe
            ));
            let v = c.temp();
            c.line(&format!("  {} = load ptr, ptr {}, align 8", v, p));
            match default {
                Some(dv) => {
                    let dvp = emit_str_val(dv, c, mdl, sigs);
                    let r = c.temp();
                    c.line(&format!(
                        "  {} = select i1 {}, ptr {}, ptr {}",
                        r, inb, v, dvp
                    ));
                    r
                }
                None => {
                    let r = c.temp();
                    c.line(&format!("  {} = select i1 {}, ptr {}, ptr null", r, inb, v));
                    r
                }
            }
        }
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
        // Clone of a string yields an OWNED copy (strdup): str results
        // are caller-freed per the owning-temp convention.
        HExpr::Clone(inner) => {
            let p = emit_str_val(inner, c, mdl, sigs);
            mdl.ensure_decl("declare ptr @strdup(ptr)");
            let d = c.temp();
            c.line(&format!("  {} = call ptr @strdup(ptr {})", d, p));
            d
        }
        other => panic!("codegen: expected a string expression, got {:?}", other),
    }
}

/// `(get vec idx [default])` — branchless bounds-checked element load. The
/// result has the vector's element type, so this is valid in int and float
/// contexts alike.
pub(crate) fn emit_get(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::Get {
        src,
        idx,
        default,
        ty,
        res,
    } = e
    else {
        panic!("codegen: emit_get on non-get")
    };
    let (d, l, elem) = vec_arg(src, c, mdl, sigs);
    let ll = mdl.elem_ll(elem);
    let iv = emit_int(idx, c, mdl, sigs);
    let inb = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, l));
    let safe = c.temp();
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 0",
        safe, inb, iv
    ));
    let p = oob_safe_elem_ptr(&inb, &safe, ll, &d, c, mdl);
    let v = c.temp();
    c.line(&mdl.elem_load(elem, &v, &p));
    match default {
        Some(dv) => {
            let dvv = emit_typed(dv, *ty, c, mdl, sigs);
            let r = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, {} {}, {} {}",
                r, inb, ll, v, ll, dvv
            ));
            r
        }
        None if *ty == Ty::I64 => {
            let r = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", r, inb, v));
            r
        }
        None => v,
    }
}

/// The two-argument `get` returns `(Option T)` materialized from the
/// in-bounds flag and the (masked) element load.
pub(crate) fn emit_get_opt(
    src: &HExpr,
    idx: &HExpr,
    adt: u32,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let (d, l, elem) = vec_arg(src, c, mdl, sigs);
    let ll = mdl.elem_ll(elem);
    let iv = emit_int(idx, c, mdl, sigs);
    let inb = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, l));
    let safe = c.temp();
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 0",
        safe, inb, iv
    ));
    let p = oob_safe_elem_ptr(&inb, &safe, ll, &d, c, mdl);
    let payload = match elem {
        Elem::I64 | Elem::F64 => {
            let v = c.temp();
            c.line(&format!("  {} = load {}, ptr {}, align 8", v, ll, p));
            v
        }
        o => panic!("Option get over {:?} elements is not supported", o),
    };
    materialize_option(c, mdl, adt, &inb, &payload)
}

pub(crate) fn emit_let(
    binds: &[(String, HExpr, Ty)],
    rebind: &[(String, usize)],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) {
    for (i, (name, value, ty)) in binds.iter().enumerate() {
        let shadowed = rebind.iter().any(|(n, idx)| n == name && *idx == i);
        if !shadowed {
            bind_one(name, value, *ty, c, mdl, sigs);
            continue;
        }
        // A rebind shadows a prior buffer of the same name. The initializer
        // may still read that buffer (`[a x a (conj a y)]` copies from the
        // old `a`), so the old storage is released only after the new value
        // exists. Snapshot the name-keyed maps before binding, put them back
        // so `free_binding_storage` sees the *predecessor*, drop it, then
        // reinstall the new entry — reusing the one tested drop routine
        // instead of a parallel release path.
        let saved = snapshot_binds(name, c);
        bind_one(name, value, *ty, c, mdl, sigs);
        let fresh = snapshot_binds(name, c);
        apply_binds(name, &saved, c);
        free_binding_storage(name, c, mdl);
        apply_binds(name, &fresh, c);
    }
}

/// The four name-keyed storage maps' entries for `name`, for save/restore
/// around a rebind's deferred release.
struct BindSnapshot {
    scalar: Option<(String, Ty)>,
    vecs: Option<(String, String, Elem)>,
    grow: Option<(String, Elem)>,
    growm: Option<(String, Ty)>,
}

fn snapshot_binds(name: &str, c: &FnCtx) -> BindSnapshot {
    BindSnapshot {
        scalar: c.scalar.get(name).cloned(),
        vecs: c.vecs.get(name).cloned(),
        grow: c.grow.get(name).cloned(),
        growm: c.growm.get(name).cloned(),
    }
}

fn apply_binds(name: &str, s: &BindSnapshot, c: &mut FnCtx) {
    set_or_remove(&mut c.scalar, name, s.scalar.clone());
    set_or_remove(&mut c.vecs, name, s.vecs.clone());
    set_or_remove(&mut c.grow, name, s.grow.clone());
    set_or_remove(&mut c.growm, name, s.growm.clone());
}

fn set_or_remove<V: Clone>(m: &mut HashMap<String, V>, name: &str, v: Option<V>) {
    match v {
        Some(v) => {
            m.insert(name.to_string(), v);
        }
        None => {
            m.remove(name);
        }
    }
}

/// Bind a single `let` binding: evaluate its initializer and install the
/// stororage under `name` in the name-keyed maps. Split out so `emit_let`
/// can release a shadowed predecessor *after* this returns.
fn bind_one(
    name: &str,
    value: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) {
    {
        // Calls returning vectors bind as plain (read-only) vec values.
        if let HExpr::Call(..) = value {
            if let Ty::Vec(el) = ty {
                let (p, l, _) = emit_vec_call(value, c, mdl, sigs);
                c.vecs.insert(name.to_string(), (p, l, el));
                return;
            }
            // Calls returning maps/sets: the %VecI64 header goes into a
            // fresh local slot (map/set ops read and owned growth rewrites
            // the local slot only).
            if matches!(ty, Ty::Map | Ty::Set) {
                let st = call_user_node(value, c, mdl, sigs);
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %VecI64, align 8", slot));
                // Only the alloca hoists: the fills read values defined in
                // the current block, so they must land there too.
                for i in 0..3 {
                    let v = c.temp();
                    c.line(&format!("  {} = extractvalue %VecI64 {}, {}", v, st, i));
                    let pf = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 {}",
                        pf, slot, i
                    ));
                    if i == 0 {
                        c.line(&format!("  store ptr {}, ptr {}, align 8", v, pf));
                    } else {
                        c.line(&format!("  store i64 {}, ptr {}, align 8", v, pf));
                    }
                }
                c.growm.insert(name.to_string(), (slot, ty));
                return;
            }
        }
        // S3 host values.
        match value {
            HExpr::ParseNum { s, int, .. } => {
                let sv = emit_str_val(s, c, mdl, sigs);
                let helper = if *int {
                    mdl.ensure_decl("declare ptr @cato_parse_i64(ptr)");
                    "cato_parse_i64"
                } else {
                    mdl.ensure_decl("declare ptr @cato_parse_f64(ptr)");
                    "cato_parse_f64"
                };
                let p = c.temp();
                c.line(&format!("  {} = call ptr @{}(ptr {})", p, helper, sv));
                c.scalar.insert(name.to_string(), (p, ty));
                return;
            }
            HExpr::Args => {
                let lptr = c.temp();
                c.prelude.push(format!("  {} = alloca i64, align 8", lptr));
                mdl.ensure_decl("declare ptr @cato_args(ptr)");
                let arr = c.temp();
                c.line(&format!("  {} = call ptr @cato_args(ptr {})", arr, lptr));
                let l = c.temp();
                c.line(&format!("  {} = load i64, ptr {}, align 8", l, lptr));
                c.vecs.insert(name.to_string(), (arr, l, Elem::Str));
                return;
            }
            _ => {}
        }
        // Option-returning lookups bind as inline ADT slots.
        if let HExpr::Get {
            src,
            idx,
            res: Some(Ty::Adt(adt)),
            ..
        } = value
        {
            let slot = emit_get_opt(src, idx, *adt, c, mdl, sigs);
            c.scalar.insert(name.to_string(), (slot, Ty::Adt(*adt)));
            return;
        }
        if let HExpr::MapGet {
            src,
            key,
            res: Some(Ty::Adt(adt)),
            ..
        } = value
        {
            let slot = emit_map_get_opt(src, key, *adt, c, mdl, sigs);
            c.scalar.insert(name.to_string(), (slot, Ty::Adt(*adt)));
            return;
        }
        if let HExpr::StrNth {
            s,
            idx,
            res: Some(Ty::Adt(adt)),
        } = value
        {
            let slot = emit_strnth_opt(s, idx, *adt, c, mdl, sigs);
            c.scalar.insert(name.to_string(), (slot, Ty::Adt(*adt)));
            return;
        }
        // Heap-constructing values bind as growable slots.
        match value {
            HExpr::Conj {
                v,
                x,
                elem,
                in_place,
            } => {
                let slot = emit_conj(v, x, *elem, *in_place, c, mdl, sigs);
                c.grow.insert(name.to_string(), (slot, *elem));
                return;
            }
            HExpr::Subvec {
                v,
                start,
                end,
                elem,
            } => {
                let slot = emit_subvec(v, start, end, *elem, c, mdl, sigs);
                c.grow.insert(name.to_string(), (slot, *elem));
                return;
            }
            HExpr::For {
                el,
                src,
                elem,
                body,
                res_elem,
            } => {
                let slot = emit_for(el, src, *elem, body, *res_elem, c, mdl, sigs);
                c.grow.insert(name.to_string(), (slot, *res_elem));
                return;
            }
            HExpr::StrCat(args) => {
                let p = emit_strcat(args, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (p, Ty::Str));
                return;
            }
            HExpr::MapLit(pairs) => {
                let slot = emit_map_lit(pairs, c, mdl, sigs);
                c.growm.insert(name.to_string(), (slot, Ty::Map));
                return;
            }
            HExpr::MapAssoc { m, k, v } => {
                let slot = emit_map_assoc(value, c, mdl, sigs);
                c.growm.insert(name.to_string(), (slot, Ty::Map));
                let _ = (m, k, v);
                return;
            }
            HExpr::SetLit(_) | HExpr::SetConj { .. } | HExpr::SetDisj { .. } => {
                let slot = match value {
                    HExpr::SetLit(vs) => emit_set_lit(vs, c, mdl, sigs),
                    HExpr::SetConj { .. } => emit_set_conj(value, c, mdl, sigs),
                    HExpr::SetDisj { .. } => emit_set_disj(value, c, mdl, sigs),
                    _ => unreachable!(),
                };
                c.growm.insert(name.to_string(), (slot, Ty::Set));
                return;
            }
            HExpr::Subs { s, start, end } => {
                let p = emit_subs(s, start, end, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (p, Ty::Str));
                return;
            }
            HExpr::Split { s, sep } => {
                let (arr, l) = emit_split(s, sep, c, mdl, sigs);
                c.vecs.insert(name.to_string(), (arr, l, Elem::Str));
                return;
            }
            HExpr::Join { sep, xs } => {
                let p = emit_join(sep, xs, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (p, Ty::Str));
                return;
            }
            _ => {}
        }
        match ty {
            Ty::F64 => {
                let v = emit_float(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, Ty::F64));
            }
            Ty::Bool => {
                let v = emit_bool(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, Ty::Bool));
            }
            Ty::Str => {
                let v = emit_str_val(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, Ty::Str));
            }
            t @ (Ty::I64
            | Ty::I32
            | Ty::I8
            | Ty::I16
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::U64
            | Ty::Usize) => {
                let v = emit_int(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, t));
            }
            Ty::F32 => {
                let v = emit_float(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, Ty::F32));
            }
            Ty::Fn(_) => {
                let slot = emit_closure_val(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (slot, ty));
            }
            Ty::Vec(_) => {
                // Destructuring temps (and plain aliases) share the source
                // vector's (ptr, len); literals materialize a static global.
                if let HExpr::Var(src) = value {
                    if let Some((slot, elem)) = c.grow.get(src).cloned() {
                        c.grow.insert(name.to_string(), (slot, elem));
                        return;
                    }
                    if let Some((p, l, elem)) = c.vecs.get(src).cloned() {
                        c.vecs.insert(name.to_string(), (p, l, elem));
                        return;
                    }
                    if let Some((slot, _)) = c.growm.get(src).cloned() {
                        c.growm.insert(name.to_string(), (slot, Ty::Map));
                        return;
                    }
                }
                // Any other vector-valued form (literal, if, match, clone,
                // conj): evaluate it and bind the (ptr, len) read-only.
                let (p, l, elem) = emit_vec_pair(value, c, mdl, sigs);
                c.vecs.insert(name.to_string(), (p, l, elem));
            }
            Ty::Adt(_) => {
                let v = emit_adt(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, ty));
            }
            Ty::Map | Ty::Set => {
                // Alias: share the source header slot (set ops read only
                // ptr/len; map ops see the same representation tag).
                if let HExpr::Var(src) = value {
                    if let Some((slot, _)) = c.growm.get(src).cloned() {
                        c.growm.insert(name.to_string(), (slot, ty));
                        return;
                    }
                }
                // Any other value form: evaluate to (ptr, len[, cap]) and
                // materialize a local header slot.
                let (p, l, cap) = if ty == Ty::Map {
                    map_val(value, c, mdl, sigs)
                } else {
                    let (p, l) = set_val(value, c, mdl, sigs);
                    (p, l, "0".to_string())
                };
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %VecI64, align 8", slot));
                // Only the alloca hoists; the header fills read values from
                // the current block.
                let pf = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
                    pf, slot
                ));
                c.line(&format!("  store ptr {}, ptr {}, align 8", p, pf));
                let lf = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
                    lf, slot
                ));
                c.line(&format!("  store i64 {}, ptr {}, align 8", l, lf));
                let cf = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
                    cf, slot
                ));
                c.line(&format!("  store i64 {}, ptr {}, align 8", cap, cf));
                c.growm.insert(name.to_string(), (slot, ty));
            }
            _ => {
                let v = emit_int(value, c, mdl, sigs);
                c.scalar.insert(name.to_string(), (v, Ty::I64));
            }
        }
    }
}

/// `let` with scope discipline: bind the names, emit the body inside that
/// scope, run the scope-exit frees, then restore everything the binds
/// shadowed — a binder must not stay visible past its `let`.
pub(crate) fn emit_let_scoped(
    binds: &[(String, HExpr, Ty)],
    frees: &[String],
    rebind: &[(String, usize)],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
    emit_body: impl FnOnce(&mut FnCtx, &mut Mdl) -> String,
) -> String {
    let names: Vec<String> = binds.iter().map(|(n, _, _)| n.clone()).collect();
    let scope = c.save_binds(&names);
    emit_let(binds, rebind, c, mdl, sigs);
    let v = emit_body(c, mdl);
    emit_let_frees(frees, c, mdl);
    c.restore_binds(scope);
    v
}

/// Evaluate an expression that yields a closure value (a Closure literal,
/// possibly wrapped in lets/seqs), binding intermediate scopes properly.
pub(crate) fn emit_closure_val(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::Closure {
            sig,
            mangled,
            captures,
            capture_owns,
            ..
        } => emit_closure_creation(mangled, *sig, captures, capture_owns, c, mdl, sigs),
        HExpr::Let(binds, body, frees, rebind) => {
            emit_let_scoped(binds, frees, rebind, c, mdl, sigs, |c, mdl| {
                emit_closure_val(body, c, mdl, sigs)
            })
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_closure_val(vs.last().unwrap(), c, mdl, sigs)
        }
        // A closure binding (its slot) or a call returning a closure.
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(r, _)| r.clone())
            .unwrap_or_else(|| panic!("codegen: {} is not a closure binding", v)),
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
        o => panic!("codegen: expected a closure value, got {:?}", o),
    }
}

/// Two-argument `(nth s i)`: `(Option i64)` from the in-bounds byte load.
pub(crate) fn emit_strnth_opt(
    s: &HExpr,
    idx: &HExpr,
    adt: u32,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let sp = emit_str_val(s, c, mdl, sigs);
    mdl.declare("declare i64 @strlen(ptr)".to_string());
    let len = c.temp();
    c.line(&format!("  {} = call i64 @strlen(ptr {})", len, sp));
    let iv = emit_int(idx, c, mdl, sigs);
    let inb = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, len));
    let safe = c.temp();
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 0",
        safe, inb, iv
    ));
    let p = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
        p, sp, safe
    ));
    let b = c.temp();
    c.line(&format!("  {} = load i8, ptr {}, align 1", b, p));
    let z = c.temp();
    c.line(&format!("  {} = zext i8 {} to i64", z, b));
    materialize_option(c, mdl, adt, &inb, &z)
}

/// The element address for a bounds-checked read: in bounds, `d[i]`;
/// out of bounds, a zeroed constant — never `d[0]`, which an empty vector
/// does not have: the caller's select discards the value, but the load
/// itself must stay in bounds. Branchless.
pub(crate) fn oob_safe_elem_ptr(
    inb: &str,
    safe: &str,
    ll: &str,
    d: &str,
    c: &mut FnCtx,
    mdl: &mut Mdl,
) -> String {
    mdl.ensure_decl("@cato_oob_zero = private constant [2 x i64] zeroinitializer, align 16");
    let p = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        p, ll, d, safe
    ));
    let q = c.temp();
    c.line(&format!(
        "  {} = select i1 {}, ptr {}, ptr @cato_oob_zero",
        q, inb, p
    ));
    q
}
