use super::*;

pub(crate) fn emit_malloc(c: &mut FnCtx, mdl: &mut Mdl, nbytes: &str) -> String {
    mdl.ensure_decl("declare ptr @malloc(i64)");
    let p = c.temp();
    c.line(&format!("  {} = call ptr @malloc(i64 {})", p, nbytes));
    p
}

/// Emit `(conj v x)`: in-place append when the owned buffer has capacity,
/// geometric growth (2x, free the old buffer) otherwise. Static sources are
/// always copied on first conj (their data is read-only). Returns the
/// growable slot; callers must register it with `c.grow`.
pub(crate) fn emit_conj(
    v: &HExpr,
    x: &HExpr,
    elem: Elem,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let ll = mdl.elem_ll(elem);
    // Source header: live loads for growable bindings; static sources get
    // capacity == length (their data is read-only, forcing the copy path).
    // Only growable (heap) sources may be freed after the copy.
    let mut freeable = false;
    let (ptr, len, cap) = match v {
        HExpr::Var(name) => {
            if let Some((slot, _)) = c.grow.get(name) {
                freeable = true;
                let slot = slot.clone();
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
                let cf = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
                    cf, slot
                ));
                let cp = c.temp();
                c.line(&format!("  {} = load i64, ptr {}, align 8", cp, cf));
                (p, l, cp)
            } else {
                let (p, l, _) = vec_arg(v, c, mdl);
                (p, l.clone(), l)
            }
        }
        _ => {
            let (p, l, _) = vec_arg(v, c, mdl);
            (p, l.clone(), l)
        }
    };

    // The appended value, in the buffer's element type.
    let xv = emit_typed(x, elem_ty_of(elem), c, mdl, sigs);
    let one = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", one, len));

    // Fast path when len < cap; grow otherwise.
    let has_room = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", has_room, len, cap));
    let bfast = c.block_name();
    let bgrow = c.block_name();
    let bmerge = c.block_name();
    c.line(&format!(
        "  br i1 {}, label %{}, label %{}",
        has_room, bfast, bgrow
    ));

    let lbl = c.open(&bfast);
    c.line(&format!("{}:", lbl));
    // The in-place append: store the element at the tail, then bump len.
    let tail = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        tail, ll, ptr, len
    ));
    match elem {
        Elem::I64 => c.line(&format!("  store i64 {}, ptr {}, align 8", xv, tail)),
        Elem::F64 => c.line(&format!("  store double {}, ptr {}, align 8", xv, tail)),
        Elem::Str => panic!("codegen: conj over string vectors not supported"),
        Elem::Param(_) => panic!("unresolved element"),
    }
    let slot_out = c.temp();
    c.prelude.push(format!(
        "  {} = alloca %VecI64, align 8",
        slot_out
    ));
    let pf2 = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
        pf2, slot_out
    ));
    c.line(&format!("  store ptr {}, ptr {}, align 8", ptr, pf2));
    let lf2 = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
        lf2, slot_out
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", one, lf2));
    let cf2 = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf2, slot_out
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", cap, cf2));
    c.line(&format!("  br label %{}", bmerge));

    let lbl = c.open(&bgrow);
    c.line(&format!("{}:", lbl));
    // newcap = max(2*cap, len+1); copy old data; append; free the old buffer.
    let dbl = c.temp();
    c.line(&format!("  {} = mul i64 {}, 2", dbl, cap));
    let ge = c.temp();
    c.line(&format!("  {} = icmp uge i64 {}, {}", ge, dbl, one));
    let newcap = c.temp();
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 {}",
        newcap, ge, dbl, one
    ));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", nbytes, newcap));
    let np = emit_malloc(c, mdl, &nbytes);
    let cbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", cbytes, len));
    mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
    c.line(&format!(
        "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
        np, ptr, cbytes
    ));
    let gp = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        gp, ll, np, len
    ));
    match elem {
        Elem::I64 => c.line(&format!("  store i64 {}, ptr {}, align 8", xv, gp)),
        Elem::F64 => c.line(&format!("  store double {}, ptr {}, align 8", xv, gp)),
        Elem::Str => panic!("codegen: conj over string vectors not supported"),
        Elem::Param(_) => panic!("unresolved element"),
    }
    if freeable {
        mdl.ensure_decl("declare void @free(ptr)");
        c.line(&format!("  call void @free(ptr {})", ptr));
    }
    let pf3 = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
        pf3, slot_out
    ));
    c.line(&format!("  store ptr {}, ptr {}, align 8", np, pf3));
    let lf3 = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
        lf3, slot_out
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", one, lf3));
    let cf3 = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf3, slot_out
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", newcap, cf3));
    c.line(&format!("  br label %{}", bmerge));

    let lbl = c.open(&bmerge);
    c.line(&format!("{}:", lbl));
    slot_out
}


/// Emit `(subvec v start end)`: a fresh heap copy of the clamped slice.
/// Branchless: bounds are clamped with selects. Returns the growable slot.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_subvec(
    v: &HExpr,
    start: &HExpr,
    end: &HExpr,
    elem: Elem,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let ll = mdl.elem_ll(elem);
    let (ptr, len, _) = vec_arg(v, c, mdl);
    let sv = emit_int(start, c, mdl, sigs);
    let ev = emit_int(end, c, mdl, sigs);
    // s' = max(0, start); e' = min(max(0, end), len); n = max(0, e'-s')
    let s0 = c.temp();
    c.line(&format!("  {} = icmp slt i64 {}, 0", s0, sv));
    let s1 = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", s1, s0, sv));
    let e0 = c.temp();
    c.line(&format!("  {} = icmp slt i64 {}, 0", e0, ev));
    let e1 = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", e1, e0, ev));
    let e2 = c.temp();
    c.line(&format!("  {} = icmp ugt i64 {}, {}", e2, e1, len));
    let e3 = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", e3, e2, len, e1));
    let d = c.temp();
    c.line(&format!("  {} = sub i64 {}, {}", d, e3, s1));
    let neg = c.temp();
    c.line(&format!("  {} = icmp slt i64 {}, 0", neg, d));
    let n = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", n, neg, d));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", nbytes, n));
    let np = emit_malloc(c, mdl, &nbytes);
    let sbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", sbytes, s1));
    mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
    c.line(&format!(
        "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
        np, ptr, sbytes
    ));
    // New growable header {np, n, n}.
    let slot = c.temp();
    c.prelude.push(format!("  {} = alloca %VecI64, align 8", slot));
    let pf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
        pf, slot
    ));
    c.line(&format!("  store ptr {}, ptr {}, align 8", np, pf));
    let lf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
        lf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", n, lf));
    let cf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", n, cf));
    let _ = ll;
    slot
}

/// Emit `(str a b ...)`: one malloc, memcpy of each part, NUL terminator.
/// Branchless. Returns the result ptr (a `Ty::Str` value).
pub(crate) fn emit_strcat(
    args: &[HExpr],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    mdl.ensure_decl("declare i64 @strlen(ptr)");
    mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
    // Gather each part's (ptr, len) — literal lengths are static.
    let mut parts: Vec<(String, String)> = Vec::new();
    for a in args {
        match a {
            HExpr::StrLit(s) => {
                let p = emit_str_val(a, c, mdl, sigs);
                let l = c.temp();
                c.line(&format!("  {} = add i64 {}, 0", l, s.bytes().count()));
                parts.push((p, l));
            }
            _ => {
                let p = emit_str_val(a, c, mdl, sigs);
                let l = c.temp();
                c.line(&format!("  {} = call i64 @strlen(ptr {})", l, p));
                parts.push((p, l));
            }
        }
    }
    // total = sum(len) + 1
    let mut total: Option<String> = None;
    for (_, l) in &parts {
        match &total {
            None => {
                let t = c.temp();
                c.line(&format!("  {} = add i64 {}, 0", t, l));
                total = Some(t);
            }
            Some(acc) => {
                let t = c.temp();
                c.line(&format!("  {} = add i64 {}, {}", t, acc, l));
                total = Some(t);
            }
        }
    }
    let total = total.unwrap_or_else(|| {
        let t = c.temp();
        c.line(&format!("  {} = add i64 0, 0", t));
        t
    });
    let nb = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", nb, total));
    let dst = emit_malloc(c, mdl, &nb);
    // Copy each part at its running offset; NUL-terminate.
    let mut off: Option<String> = None;
    for (p, l) in &parts {
        let dst_at = match &off {
            None => dst.clone(),
            Some(o) => {
                let t = c.temp();
                c.line(&format!("  {} = getelementptr inbounds i8, ptr {}, i64 {}", t, dst, o));
                t
            }
        };
        c.line(&format!(
            "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
            dst_at, p, l
        ));
        match &off {
            None => {
                let t = c.temp();
                c.line(&format!("  {} = add i64 {}, 0", t, l));
                off = Some(t);
            }
            Some(acc) => {
                let t = c.temp();
                c.line(&format!("  {} = add i64 {}, {}", t, acc, l));
                off = Some(t);
            }
        }
    }
    let end_at = match &off {
        None => dst.clone(),
        Some(o) => {
            let t = c.temp();
            c.line(&format!("  {} = getelementptr inbounds i8, ptr {}, i64 {}", t, dst, o));
            t
        }
    };
    c.line(&format!("  store i8 0, ptr {}", end_at));
    dst
}


// ---------------------------------------------------------------------------
// subs / split / join: heap string building.
// ---------------------------------------------------------------------------

/// `(subs s start end)` — a fresh heap string holding bytes [start, end)
/// plus a NUL terminator.
pub(crate) fn emit_subs(
    s: &HExpr,
    start: &HExpr,
    end: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let sp = emit_str_val(s, c, mdl, sigs);
    let st = emit_int(start, c, mdl, sigs);
    let en = emit_int(end, c, mdl, sigs);
    // len = end - start (clamped up to 0 for empty slices).
    let diff = c.temp();
    c.line(&format!("  {} = sub i64 {}, {}", diff, en, st));
    let neg = c.temp();
    c.line(&format!("  {} = icmp slt i64 {}, 0", neg, diff));
    let len = c.temp();
    c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", len, neg, diff));
    let one = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", one, len));
    let np = emit_malloc(c, mdl, &one);
    mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
    let spoff = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
        spoff, sp, st
    ));
    c.line(&format!(
        "  call void @llvm.memcpy.p0.p0.i64(ptr {}, ptr {}, i64 {}, i1 false, i1 false)",
        np, spoff, len
    ));
    // NUL terminator.
    let tail = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
        tail, np, len
    ));
    c.line(&format!("  store i8 0, ptr {}, align 1", tail));
    np
}

/// `(split s sep)` — a fresh heap vector of strings. The C runtime returns a
/// malloc'd array of malloc'd NUL-terminated strings; the length comes back
/// through an out-param.
pub(crate) fn emit_split(
    s: &HExpr,
    sep: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String) {
    mdl.ensure_decl("declare ptr @cato_str_split(ptr, ptr, ptr)");
    let sp = emit_str_val(s, c, mdl, sigs);
    let sepp = emit_str_val(sep, c, mdl, sigs);
    let out = c.temp();
    c.prelude.push(format!("  {} = alloca i64, align 8", out));
    let arr = c.temp();
    c.line(&format!(
        "  {} = call ptr @cato_str_split(ptr {}, ptr {}, ptr {})",
        arr, sp, sepp, out
    ));
    let lf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 0",
        lf, out
    ));
    let l = c.temp();
    c.line(&format!("  {} = load i64, ptr {}, align 8", l, lf));
    (arr, l)
}

/// `(join sep xs)` — a fresh heap string joining a (Vector str) with sep.
pub(crate) fn emit_join(
    sep: &HExpr,
    xs: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    mdl.ensure_decl("declare ptr @cato_str_join(ptr, i64, ptr)");
    let sepp = emit_str_val(sep, c, mdl, sigs);
    let (d, l, _) = vec_arg(xs, c, mdl);
    let p = c.temp();
    c.line(&format!(
        "  {} = call ptr @cato_str_join(ptr {}, i64 {}, ptr {})",
        p, d, l, sepp
    ));
    p
}
