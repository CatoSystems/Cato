use super::*;

/// Store a `{ptr, len, cap}` header into an alloca'd slot (the set / map
/// header shape, shared with `%VecI64`).
pub(crate) fn store_header(slot: &str, p: &str, len: &str, c: &mut FnCtx) {
    c.prelude
        .push(format!("  {} = alloca %VecI64, align 8", slot));
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
    c.line(&format!("  store i64 {}, ptr {}, align 8", len, lf));
    let cf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", len, cf));
}

/// Set value in (ptr, len) position: a growm binding, a literal, or a
/// freshly conj'ed / disj'ed set.
pub(crate) fn set_val(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String) {
    match e {
        HExpr::Var(v) => {
            if let Some((slot, _)) = c.growm.get(v) {
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
                (p, l)
            } else {
                panic!("codegen: {} is not a set binding", v)
            }
        }
        // A call returning a set: the %VecI64 header comes back by value.
        HExpr::Call(..) => {
            let st = call_user_node(e, c, mdl, sigs);
            let p = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 0", p, st));
            let l = c.temp();
            c.line(&format!("  {} = extractvalue %VecI64 {}, 1", l, st));
            (p, l)
        }
        HExpr::SetLit(_) | HExpr::SetConj { .. } | HExpr::SetDisj { .. } => {
            let slot = match e {
                HExpr::SetLit(vs) => emit_set_lit(vs, c, mdl, sigs),
                HExpr::SetConj { .. } => emit_set_conj(e, c, mdl, sigs),
                HExpr::SetDisj { .. } => emit_set_disj(e, c, mdl, sigs),
                _ => unreachable!(),
            };
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
            (p, l)
        }
        other => panic!("codegen: unsupported set value {:?}", other),
    }
}

/// Emit a set literal: elements are sorted and deduped at compile time;
/// one malloc, stride 1.
pub(crate) fn emit_set_lit(
    vals: &[HExpr],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let n = vals.len();
    let nreg = c.temp();
    c.line(&format!("  {} = add i64 {}, 0", nreg, n));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", nbytes, nreg));
    let p = emit_malloc(c, mdl, &nbytes);
    for (i, v) in vals.iter().enumerate() {
        let vv = emit_int(v, c, mdl, sigs);
        let f = c.temp();
        c.line(&format!(
            "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
            f, p, i
        ));
        c.line(&format!("  store i64 {}, ptr {}, align 8", vv, f));
    }
    let slot = c.temp();
    store_header(&slot, &p, &nreg, c);
    slot
}

/// Shared filter-copy core for conj/disj: copy every entry whose value
/// `!= x` into `np` with a write pointer. Returns (write-pointer-out
/// register name, loop blocks wired). The caller appends and sets len.
fn emit_set_filter_copy(ptr: &str, len: &str, xv: &str, np: &str, c: &mut FnCtx) -> String {
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
    let wvar = c.temp();
    let inext = c.temp();
    let f = c.temp();
    let vreg = c.temp();
    let keep = c.temp();
    let wnext = c.temp();
    let dst = c.temp();
    let lbl = c.open(&bloop);
    c.line(&format!("{}:", lbl));
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        ivar, pre, inext, bloop
    ));
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        wvar, pre, wnext, bloop
    ));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        f, ptr, ivar
    ));
    c.line(&format!("  {} = load i64, ptr {}, align 8", vreg, f));
    c.line(&format!("  {} = icmp ne i64 {}, {}", keep, vreg, xv));
    // Always write at the cursor; it only advances when kept.
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        dst, np, wvar
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", vreg, dst));
    let wplus = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", wplus, wvar));
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 {}",
        wnext, keep, wplus, wvar
    ));
    c.line(&format!("  {} = add i64 {}, 1", inext, ivar));
    let cond = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", cond, inext, len));
    c.line(&format!(
        "  br i1 {}, label %{}, label %{}",
        cond, bloop, bexit
    ));

    let lbl = c.open(&bexit);
    c.line(&format!("{}:", lbl));
    let wout = c.temp();
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        wout, pre, wnext, bloop
    ));
    wout
}

/// Emit `(conj s x)` on a set: copy entries != x, append x (dedup).
pub(crate) fn emit_set_conj(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::SetConj { s, x } = e else {
        panic!("codegen: emit_set_conj on non-conj")
    };
    let (ptr, len) = set_val(s, c, mdl, sigs);
    let xv = emit_int(x, c, mdl, sigs);
    let cap = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", cap, len));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", nbytes, cap));
    let np = emit_malloc(c, mdl, &nbytes);
    let wout = emit_set_filter_copy(&ptr, &len, &xv, &np, c);
    let app = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        app, np, wout
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", xv, app));
    let newlen = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", newlen, wout));
    let slot = c.temp();
    store_header(&slot, &np, &newlen, c);
    slot
}

/// Emit `(disj s x)` on a set: copy entries != x.
pub(crate) fn emit_set_disj(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::SetDisj { s, x } = e else {
        panic!("codegen: emit_set_disj on non-disj")
    };
    let (ptr, len) = set_val(s, c, mdl, sigs);
    let xv = emit_int(x, c, mdl, sigs);
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", nbytes, len));
    let np = emit_malloc(c, mdl, &nbytes);
    let wout = emit_set_filter_copy(&ptr, &len, &xv, &np, c);
    let slot = c.temp();
    store_header(&slot, &np, &wout, c);
    slot
}

/// Emit `(contains? s x)`: branchless linear scan (last match wins, but
/// sets are deduped so at most one exists).
pub(crate) fn emit_set_contains(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::SetContains { s, x } = e else {
        panic!("codegen: emit_set_contains on non-contains")
    };
    let (ptr, len) = set_val(s, c, mdl, sigs);
    let xv = emit_int(x, c, mdl, sigs);
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
    let f = c.temp();
    let vreg = c.temp();
    let m = c.temp();
    let rphi = c.temp();
    let res2 = c.temp();
    let lbl = c.open(&bloop);
    c.line(&format!("{}:", lbl));
    c.line(&format!(
        "  {} = phi i1 [ false, %{} ], [ {}, %{} ]",
        rphi, pre, res2, bloop
    ));
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        ivar, pre, inext, bloop
    ));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        f, ptr, ivar
    ));
    c.line(&format!("  {} = load i64, ptr {}, align 8", vreg, f));
    c.line(&format!("  {} = icmp eq i64 {}, {}", m, vreg, xv));
    c.line(&format!("  {} = or i1 {}, {}", res2, m, rphi));
    c.line(&format!("  {} = add i64 {}, 1", inext, ivar));
    let cond = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", cond, inext, len));
    c.line(&format!(
        "  br i1 {}, label %{}, label %{}",
        cond, bloop, bexit
    ));

    let lbl = c.open(&bexit);
    c.line(&format!("{}:", lbl));
    let out = c.temp();
    c.line(&format!(
        "  {} = phi i1 [ false, %{} ], [ {}, %{} ]",
        out, pre, res2, bloop
    ));
    out
}
