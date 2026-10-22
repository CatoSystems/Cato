use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_for(
    el: &str,
    src: &HExpr,
    elem: Elem,
    body: &HExpr,
    res_elem: Elem,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let (ptr, len, _) = vec_arg(src, c, mdl, sigs);
    let rll = mdl.elem_ll(res_elem);
    let ll = mdl.elem_ll(elem);
    let nbytes = c.temp();
    c.line(&format!(
        "  {} = mul i64 {}, {}",
        nbytes,
        len,
        res_elem.bytes()
    ));
    let res = emit_malloc(c, mdl, &nbytes);

    let bloop = c.block_name();
    let bexit = c.block_name();
    let is_empty = c.temp();
    let pre = c.cur.clone();
    c.line(&format!("  {} = icmp eq i64 {}, 0", is_empty, len));
    c.line(&format!(
        "  br i1 {}, label %{}, label %{}",
        is_empty, bexit, bloop
    ));

    let ivar = c.temp();
    let inext = c.temp();
    let pvar = c.temp();
    let xvar = c.temp();
    let lbl = c.open(&bloop);
    c.line(&format!("{}:", lbl));
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        ivar, pre, inext, bloop
    ));
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        pvar, ll, ptr, ivar
    ));
    c.line(&mdl.elem_load(elem, &xvar, &pvar));
    c.scalar.insert(el.to_string(), (xvar, elem_ty_of(elem)));
    let val = emit_typed(body, elem_ty_of(res_elem), c, mdl, sigs);
    let w = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds {}, ptr {}, i64 {}",
        w, rll, res, ivar
    ));
    match res_elem {
        Elem::Str => panic!("codegen: for over strings not supported"),
        Elem::Param(_) => panic!("unresolved element"),
        e => c.line(&mdl.elem_store(e, &val, &w)),
    }
    c.line(&format!("  {} = add i64 {}, 1", inext, ivar));
    let cond = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", cond, inext, len));
    if mdl.fast {
        c.line(&format!(
            "  br i1 {}, label %{}, label %{}, !llvm.loop !0",
            cond, bloop, bexit
        ));
    } else {
        c.line(&format!(
            "  br i1 {}, label %{}, label %{}",
            cond, bloop, bexit
        ));
    }

    let lbl = c.open(&bexit);
    c.line(&format!("{}:", lbl));
    // Result header {res, len, len}.
    let slot = c.temp();
    c.prelude
        .push(format!("  {} = alloca %VecI64, align 8", slot));
    let pf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 0",
        pf, slot
    ));
    c.line(&format!("  store ptr {}, ptr {}, align 8", res, pf));
    let lf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
        lf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", len, lf));
    let cf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", len, cf));
    slot
}

// ---------------------------------------------------------------------------
// Maps: flat arrays of (i64 key, i64 value) pairs on the heap. Entries are
// 16 bytes (key + value); headers are growable {ptr, len(=entries), cap}.
// ---------------------------------------------------------------------------
