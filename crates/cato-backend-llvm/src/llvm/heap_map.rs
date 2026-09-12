use super::*;

pub(crate) fn emit_map_get(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::MapGet { src, key, default } = e else {
        panic!("codegen: emit_map_get on non-mapget")
    };
    let (ptr, len) = map_val(src, c, mdl, sigs);
    let kv = emit_int(key, c, mdl, sigs);
    // res starts at the default (or 0); each match overwrites it, so the
    // last matching entry wins.
    let res = match default {
        Some(d) => emit_int(d, c, mdl, sigs),
        None => {
            let t = c.temp();
            c.line(&format!("  {} = add i64 0, 0", t));
            t
        }
    };
    let bloop = c.block_name();
    let bexit = c.block_name();
    let is_empty = c.temp();
    let pre = c.cur.clone();
    c.line(&format!("  {} = icmp eq i64 {}, 0", is_empty, len));
    c.line(&format!("  br i1 {}, label %{}, label %{}", is_empty, bexit, bloop));

    let ivar = c.temp();
    let inext = c.temp();
    let kf = c.temp();
    let kreg = c.temp();
    let vf = c.temp();
    let vreg = c.temp();
    let m = c.temp();
    let rphi = c.temp();
    let res2 = c.temp();
    let lbl = c.open(&bloop);
    c.line(&format!("{}:", lbl));
    // Loop-carried running result: each match overwrites it, so the last
    // matching entry wins (assoc replaces).
    c.line(&format!(
        "  {} = phi i64 [ {}, %{} ], [ {}, %{} ]",
        rphi, res, pre, res2, bloop
    ));
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        ivar, pre, inext, bloop
    ));
    let twoi = c.temp();
    c.line(&format!("  {} = mul i64 {}, 2", twoi, ivar));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        kf, ptr, twoi
    ));
    c.line(&format!("  {} = load i64, ptr {}, align 8", kreg, kf));
    let one = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", one, twoi));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        vf, ptr, one
    ));
    c.line(&format!("  {} = load i64, ptr {}, align 8", vreg, vf));
    c.line(&format!("  {} = icmp eq i64 {}, {}", m, kreg, kv));
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 {}",
        res2, m, vreg, rphi
    ));
    c.line(&format!("  {} = add i64 {}, 1", inext, ivar));
    let cond = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", cond, inext, len));
    c.line(&format!("  br i1 {}, label %{}, label %{}", cond, bloop, bexit));

    let lbl = c.open(&bexit);
    c.line(&format!("{}:", lbl));
    let out = c.temp();
    c.line(&format!(
        "  {} = phi i64 [ {}, %{} ], [ {}, %{} ]",
        out, res, pre, res2, bloop
    ));
    out
}

/// Map value in (ptr, entries) position: growable binding or literal.
pub(crate) fn map_val(
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
                panic!("codegen: {} is not a map binding", v)
            }
        }
        HExpr::MapLit(pairs) => {
            let slot = emit_map_lit(pairs, c, mdl, sigs);
            let (p, l) = {
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
            };
            (p, l)
        }
        HExpr::MapAssoc { .. } => {
            let slot = emit_map_assoc(e, c, mdl, sigs);
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
        other => panic!("codegen: unsupported map value {:?}", other),
    }
}

/// Emit a map literal: entries sorted at compile time; one malloc.
pub(crate) fn emit_map_lit(
    pairs: &[(HExpr, HExpr)],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let n = pairs.len();
    let nreg = c.temp();
    c.line(&format!("  {} = add i64 {}, 0", nreg, n));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 16", nbytes, nreg));
    let p = emit_malloc(c, mdl, &nbytes);
    for (i, (k, v)) in pairs.iter().enumerate() {
        let kv = emit_int(k, c, mdl, sigs);
        let vv = emit_int(v, c, mdl, sigs);
        let kf = c.temp();
        c.line(&format!(
            "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
            kf, p, i * 2
        ));
        c.line(&format!("  store i64 {}, ptr {}, align 8", kv, kf));
        let vf = c.temp();
        c.line(&format!(
            "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
            vf, p, i * 2 + 1
        ));
        c.line(&format!("  store i64 {}, ptr {}, align 8", vv, vf));
    }
    let slot = c.temp();
    c.prelude.push(format!("  {} = alloca %VecI64, align 8", slot));
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
    c.line(&format!("  store i64 {}, ptr {}, align 8", nreg, lf));
    let cf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", nreg, cf));
    slot
}

/// Emit `(assoc m k v)`: a fresh buffer holding every old entry whose key
/// differs (single filter-copy pass with a write pointer), then (k, v)
/// appended — so an existing key is replaced and count stays honest.
pub(crate) fn emit_map_assoc(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::MapAssoc { m, k, v } = e else {
        panic!("codegen: emit_map_assoc on non-assoc")
    };
    let (ptr, len) = map_val(m, c, mdl, sigs);
    let kv = emit_int(k, c, mdl, sigs);
    let vv = emit_int(v, c, mdl, sigs);
    // Capacity len+1 covers the replace (<= len kept) plus the new entry.
    let cap = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", cap, len));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 16", nbytes, cap));
    let np = emit_malloc(c, mdl, &nbytes);

    // Filter-copy loop: keep entries whose key != kv, compacting writes.
    let bloop = c.block_name();
    let bexit = c.block_name();
    let is_empty = c.temp();
    let pre = c.cur.clone();
    c.line(&format!("  {} = icmp eq i64 {}, 0", is_empty, len));
    c.line(&format!("  br i1 {}, label %{}, label %{}", is_empty, bexit, bloop));

    let ivar = c.temp();
    let wvar = c.temp();
    let inext = c.temp();
    let kf = c.temp();
    let kreg = c.temp();
    let vf = c.temp();
    let vreg = c.temp();
    let keep = c.temp();
    let wnext = c.temp();
    let dstk = c.temp();
    let dstv = c.temp();
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
    let twoi = c.temp();
    c.line(&format!("  {} = mul i64 {}, 2", twoi, ivar));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        kf, ptr, twoi
    ));
    c.line(&format!("  {} = load i64, ptr {}, align 8", kreg, kf));
    let onei = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", onei, twoi));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        vf, ptr, onei
    ));
    c.line(&format!("  {} = load i64, ptr {}, align 8", vreg, vf));
    c.line(&format!("  {} = icmp ne i64 {}, {}", keep, kreg, kv));
    // Always write at the write cursor; it only advances when kept.
    let two_w = c.temp();
    c.line(&format!("  {} = mul i64 {}, 2", two_w, wvar));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        dstk, np, two_w
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", kreg, dstk));
    let one_w = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", one_w, two_w));
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        dstv, np, one_w
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", vreg, dstv));
    let wplus = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", wplus, wvar));
    c.line(&format!(
        "  {} = select i1 {}, i64 {}, i64 {}",
        wnext, keep, wplus, wvar
    ));
    c.line(&format!("  {} = add i64 {}, 1", inext, ivar));
    let cond = c.temp();
    c.line(&format!("  {} = icmp ult i64 {}, {}", cond, inext, len));
    c.line(&format!("  br i1 {}, label %{}, label %{}", cond, bloop, bexit));

    let lbl = c.open(&bexit);
    c.line(&format!("{}:", lbl));
    // The write pointer phi: after the loop it holds the kept count.
    let wout = c.temp();
    c.line(&format!(
        "  {} = phi i64 [ 0, %{} ], [ {}, %{} ]",
        wout, pre, wnext, bloop
    ));
    // Append (k, v) at the write cursor.
    let two_w2 = c.temp();
    c.line(&format!("  {} = mul i64 {}, 2", two_w2, wout));
    let appk = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        appk, np, two_w2
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", kv, appk));
    let one_w2 = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", one_w2, two_w2));
    let appv = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
        appv, np, one_w2
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", vv, appv));
    let newlen = c.temp();
    c.line(&format!("  {} = add i64 {}, 1", newlen, wout));

    // New header {np, newlen, newlen}.
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
    c.line(&format!("  store i64 {}, ptr {}, align 8", newlen, lf));
    let cf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 2",
        cf, slot
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", newlen, cf));
    slot
}
