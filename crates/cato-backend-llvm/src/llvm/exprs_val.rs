use super::*;


pub(crate) fn materialize_vec_lit(value: &HExpr, name: &str, c: &mut FnCtx, mdl: &mut Mdl) {
    let HExpr::VecLit(lit) = value else {
        panic!("codegen: can only materialize vector literal bindings")
    };
    let (label, len, elem) = mdl.mint_vec(lit);
    let t_v = c.temp();
    c.line(&format!("  {} = alloca %VecI64, align 8", t_v));
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
/// bindings first.

pub(crate) fn vec_arg(expr: &HExpr, c: &mut FnCtx, mdl: &mut Mdl) -> (String, String, Elem) {
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
            c.line(&format!("  {} = alloca %VecI64, align 8", t_v));
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
        _ => panic!("codegen: unsupported vector argument"),
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
            let n = s.bytes().count() + 1;
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
        HExpr::Subs { s, start, end } => {
            let p = emit_subs(s, start, end, c, mdl, sigs);
            p
        }
        HExpr::Join { sep, xs } => emit_join(sep, xs, c, mdl, sigs),
        HExpr::Get {
            src,
            idx,
            default,
            ty: Ty::Str,
        } => {
            // Element load from a (Vector str): bounds-checked, null (or the
            // default string) out of range.
            let (d, l, _) = vec_arg(src, c, mdl);
            let iv = emit_int(idx, c, mdl, sigs);
            let inb = c.temp();
            c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, l));
            let safe = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", safe, inb, iv));
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
    } = e
    else {
        panic!("codegen: emit_get on non-get")
    };
    let (d, l, elem) = vec_arg(src, c, mdl);
    let ll = mdl.elem_ll(elem);
    let iv = emit_int(idx, c, mdl, sigs);
    let inb = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, l));
    let safe = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", safe, inb, iv));
    let p = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        p, ll, d, safe
    ));
    let v = c.temp();
    c.line(&format!("  {} = load {}, ptr {}, align 8", v, ll, p));
    match default {
        Some(dv) => {
            let dvv = emit_typed(dv, *ty, c, mdl, sigs);
            let r = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, {} {}, {} {}",
                r,
                inb,
                ll,
                v,
                ll,
                dvv
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

pub(crate) fn emit_let(binds: &[(String, HExpr, Ty)], c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
    for (name, value, ty) in binds {
        // Calls returning vectors bind as plain (read-only) vec values.
        if let HExpr::Call(..) = value {
            if let Ty::Vec(el) = *ty {
                let (p, l, _) = emit_vec_call(value, c, mdl, sigs);
                c.vecs.insert(name.clone(), (p, l, el));
                continue;
            }
        }
        // Heap-constructing values bind as growable slots.
        match value {
            HExpr::Conj { v, x, elem } => {
                let slot = emit_conj(v, x, *elem, c, mdl, sigs);
                c.grow.insert(name.clone(), (slot, *elem));
                continue;
            }
            HExpr::Subvec {
                v,
                start,
                end,
                elem,
            } => {
                let slot = emit_subvec(v, start, end, *elem, c, mdl, sigs);
                c.grow.insert(name.clone(), (slot, *elem));
                continue;
            }
            HExpr::For {
                el,
                src,
                elem,
                body,
                res_elem,
            } => {
                let slot = emit_for(el, src, *elem, body, *res_elem, c, mdl, sigs);
                c.grow.insert(name.clone(), (slot, *res_elem));
                continue;
            }
            HExpr::StrCat(args) => {
                let p = emit_strcat(args, c, mdl, sigs);
                c.scalar.insert(name.clone(), (p, Ty::Str));
                continue;
            }
            HExpr::MapLit(pairs) => {
                let slot = emit_map_lit(pairs, c, mdl, sigs);
                c.growm.insert(name.clone(), (slot, Ty::Map));
                continue;
            }
            HExpr::MapAssoc { m, k, v } => {
                let slot = emit_map_assoc(value, c, mdl, sigs);
                c.growm.insert(name.clone(), (slot, Ty::Map));
                let _ = (m, k, v);
                continue;
            }
            HExpr::SetLit(_) | HExpr::SetConj { .. } | HExpr::SetDisj { .. } => {
                let slot = match value {
                    HExpr::SetLit(vs) => emit_set_lit(vs, c, mdl, sigs),
                    HExpr::SetConj { .. } => emit_set_conj(value, c, mdl, sigs),
                    HExpr::SetDisj { .. } => emit_set_disj(value, c, mdl, sigs),
                    _ => unreachable!(),
                };
                c.growm.insert(name.clone(), (slot, Ty::Set));
                continue;
            }
            HExpr::Subs { s, start, end } => {
                let p = emit_subs(s, start, end, c, mdl, sigs);
                c.scalar.insert(name.clone(), (p, Ty::Str));
                continue;
            }
            HExpr::Split { s, sep } => {
                let (arr, l) = emit_split(s, sep, c, mdl, sigs);
                c.vecs.insert(name.clone(), (arr, l, Elem::Str));
                continue;
            }
            HExpr::Join { sep, xs } => {
                let p = emit_join(sep, xs, c, mdl, sigs);
                c.scalar.insert(name.clone(), (p, Ty::Str));
                continue;
            }
            _ => {}
        }
        match ty {
            Ty::F64 => {
                let v = emit_float(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::F64));
            }
            Ty::Bool => {
                let v = emit_bool(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::Bool));
            }
            Ty::Str => {
                let v = emit_str_val(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::Str));
            }
            t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
                let v = emit_int(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, *t));
            }
            Ty::F32 => {
                let v = emit_float(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::F32));
            }
            Ty::Fn(_) => {
                let slot = emit_closure_val(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (slot, *ty));
            }
            Ty::Vec(_) => {
                // Destructuring temps (and plain aliases) share the source
                // vector's (ptr, len); literals materialize a static global.
                if let HExpr::Var(src) = value {
                    if let Some((slot, elem)) = c.grow.get(src).cloned() {
                        c.grow.insert(name.clone(), (slot, elem));
                        continue;
                    }
                    if let Some((p, l, elem)) = c.vecs.get(src).cloned() {
                        c.vecs.insert(name.clone(), (p, l, elem));
                        continue;
                    }
                    if let Some((slot, _)) = c.growm.get(src).cloned() {
                        c.growm.insert(name.clone(), (slot, Ty::Map));
                        continue;
                    }
                }
                materialize_vec_lit(value, name, c, mdl)
            }
            Ty::Adt(_) => {
                let v = emit_adt(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, *ty));
            }
            _ => {
                let v = emit_int(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::I64));
            }
        }
    }
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
        HExpr::Closure { sig, mangled, captures, .. } => {
            emit_closure_creation(mangled, *sig, captures, c, mdl, sigs)
        }
        HExpr::Let(binds, body, frees) => {
            emit_let(binds, c, mdl, sigs);
            let v = emit_closure_val(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_closure_val(vs.last().unwrap(), c, mdl, sigs)
        }
        o => panic!("codegen: expected a closure value, got {:?}", o),
    }
}
