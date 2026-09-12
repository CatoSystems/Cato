use super::*;

pub(crate) fn emit_loop(spec: &LoopSpec, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    let mut adt_caps: Vec<(String, u32)> = Vec::new();
    for (n, t) in &spec.caps {
        if let Ty::Adt(i) = t {
            adt_caps.push((n.clone(), *i));
        } else {
            c.scalar.insert(n.clone(), (format!("%{}", mangle(n)), *t));
        }
    }
    let acc_ll = if spec.acc_ty == Ty::F64 { "double" } else { "i64" };
    let init_val = loop_init(&spec.reduce);
    let init_lit = if spec.acc_ty == Ty::F64 {
        llvm_f64(init_val)
    } else {
        init_val.to_string()
    };

    // Source wrappers in application order (innermost first) and the base.
    let (wrappers, base) = unwrap_source(reduce_src(&spec.reduce));

    // Helper signature: one data/len pair per base vector variable, then
    // captures. `range` bases contribute no parameters (their bounds are
    // ordinary captures evaluated in the entry block).
    let mut params: Vec<String> = Vec::new();
    match (base, spec.base_elems.len()) {
        (HSource::Var(_) | HSource::Lit(_), 1) => {
            params.push("ptr noalias nocapture readonly %data".to_string());
            params.push("i64 %len".to_string());
        }
        (HSource::Zip(..), 2) => {
            params.push("ptr noalias nocapture readonly %dataA".to_string());
            params.push("i64 %lenA".to_string());
            params.push("ptr noalias nocapture readonly %dataB".to_string());
            params.push("i64 %lenB".to_string());
        }
        (HSource::Range(..), _) => {}
        _ => unreachable!("base source / element count"),
    }
    for (n, t) in &spec.caps {
        let ll = match t {
            Ty::Adt(i) => mdl.adt_name(*i),
            other => mdl.ll_ty(*other),
        };
        params.push(format!("{} %{}", ll, mangle(n)));
    }
    for (n, i) in &adt_caps {
        let r = format!("%{}", mangle(n));
        let slot = c.temp();
        c.prelude
            .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(*i)));
        c.prelude.push(format!(
            "  store {} {}, ptr {}, align 8",
            mdl.adt_name(*i),
            r,
            slot
        ));
        c.scalar.insert(n.clone(), (slot, Ty::Adt(*i)));
    }

    c.line("entry:");
    c.open("entry");

    // Compute the iteration start index and element count. take/drop/range
    // bounds are emitted here (branchlessly), so the whole pipeline still
    // compiles to one loop.
    let (mut start_v, mut count_v): (String, String) = match base {
        HSource::Var(_) | HSource::Lit(_) => ("0".to_string(), "%len".to_string()),
        HSource::Zip(..) => {
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 %lenA, %lenB", lt));
            let cnt = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, i64 %lenA, i64 %lenB",
                cnt, lt
            ));
            ("0".to_string(), cnt)
        }
        HSource::Range(a, b) => {
            let sv = emit_int(a, &mut c, mdl, sigs);
            let ev = emit_int(b, &mut c, mdl, sigs);
            let d = c.temp();
            c.line(&format!("  {} = sub i64 {}, {}", d, ev, sv));
            let neg = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, 0", neg, d));
            let cnt = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", cnt, neg, d));
            (sv, cnt)
        }
        _ => unreachable!("base source"),
    };
    for (n, is_drop) in &wrappers {
        let nv = emit_int(n, &mut c, mdl, sigs);
        if *is_drop {
            let s1 = c.temp();
            c.line(&format!("  {} = add i64 {}, {}", s1, start_v, nv));
            let c1 = c.temp();
            c.line(&format!("  {} = sub i64 {}, {}", c1, count_v, nv));
            let neg = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, 0", neg, c1));
            let c1c = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 0, i64 {}", c1c, neg, c1));
            start_v = s1;
            count_v = c1c;
        } else {
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, {}", lt, nv, count_v));
            let c2 = c.temp();
            c.line(&format!(
                "  {} = select i1 {}, i64 {}, i64 {}",
                c2, lt, nv, count_v
            ));
            count_v = c2;
        }
    }
    let t_isempty = c.temp();
    c.line(&format!("  {} = icmp eq i64 {}, 0", t_isempty, count_v));
    c.line(&format!("  br i1 {}, label %ret, label %loop", t_isempty));

    // Element / accumulator bindings shared by all stages.
    let (el, el2) = match &spec.reduce {
        HReduce::Closure { el, el2, .. } => (el.clone(), el2.clone()),
        HReduce::Pipeline { maps, filters, .. } => {
            let (first_el, first_el2) = match (maps.first(), filters.first()) {
                (Some(m), _) => (Some(&m.el), m.el2.clone()),
                (None, Some(f)) => (Some(&f.el), f.el2.clone()),
                (None, None) => (None, None),
            };
            (
                first_el.cloned().unwrap_or_else(|| "x".to_string()),
                first_el2,
            )
        }
    };
    let accsym = match &spec.reduce {
        HReduce::Closure { acc, .. } => acc.clone(),
        _ => "acc".to_string(),
    };

    // Register defined in the loop body that carries the accumulator to the
    // next iteration (and into `ret`). Set by each branch below.
    let carried: String;
    let carried_block: &str;

    let (filters, maps) = match &spec.reduce {
        HReduce::Closure { maps, filters, .. } | HReduce::Pipeline { maps, filters, .. } => {
            (filters, maps)
        }
    };

    // Element loads for the base kind, at index start + i.
    // Returns (%x-reg, Option(%x2-reg)).
    pub(crate) fn elem_loads(
        c: &mut FnCtx,
        base: &HSource,
        start_v: &str,
        elems: &[Elem],
        mdl: &Mdl,
    ) -> (String, Ty, Option<(String, Ty)>) {
        let ll = |e: Elem| mdl.elem_ll(e);
        match base {
            HSource::Var(_) | HSource::Lit(_) => {
                let e = elems.first().copied().unwrap_or(Elem::I64);
                let pidx = c.temp();
                c.line(&format!("  {} = add i64 {}, %i", pidx, start_v));
                let p = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr %data, i64 {}",
                    p,
                    ll(e),
                    pidx
                ));
                let x = c.temp();
                c.line(&format!(
                    "  {} = load {}, ptr {}, align 8",
                    x,
                    ll(e),
                    p
                ));
                (x, elem_ty_of(e), None)
            }
            HSource::Zip(..) => {
                let ea = elems.first().copied().unwrap_or(Elem::I64);
                let eb = elems.get(1).copied().unwrap_or(Elem::I64);
                let pidx = c.temp();
                c.line(&format!("  {} = add i64 {}, %i", pidx, start_v));
                let pa = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr %dataA, i64 {}",
                    pa,
                    ll(ea),
                    pidx
                ));
                let xa = c.temp();
                c.line(&format!("  {} = load {}, ptr {}, align 8", xa, ll(ea), pa));
                let pb = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds {}, ptr %dataB, i64 {}",
                    pb,
                    ll(eb),
                    pidx
                ));
                let xb = c.temp();
                c.line(&format!("  {} = load {}, ptr {}, align 8", xb, ll(eb), pb));
                (
                    xa,
                    elem_ty_of(ea),
                    Some((xb, elem_ty_of(eb))),
                )
            }
            HSource::Range(..) => {
                let x = c.temp();
                c.line(&format!("  {} = add i64 {}, %i", x, start_v));
                (x, Ty::I64, None)
            }
            _ => unreachable!("base source"),
        }
    }

    match &spec.reduce {
        // Single-block closure: plain body, no stages, no control flow.
        HReduce::Closure { body, .. }
            if maps.is_empty() && filters.is_empty() && !has_ctl(body) =>
        {
            // Compute the loop-carried accumulator value first (its register
            // name becomes the phi's back-edge operand), then emit the block.
            let mut scratch = c.clone_with_lines(Vec::new());
            scratch.cur = "%loop".to_string();
            let (xv, xty, x2v) = elem_loads(&mut scratch, base, &start_v, &spec.base_elems, mdl);
            scratch.scalar.insert(el.clone(), (xv.clone(), xty));
            if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                scratch.scalar.insert(name.clone(), (reg.clone(), *ty));
            }
            scratch
                .scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let val = if spec.acc_ty == Ty::F64 {
                emit_float(body, &mut scratch, mdl, sigs)
            } else {
                emit_int(body, &mut scratch, mdl, sigs)
            };
            c.counter = scratch.counter;
            c.line("");
            c.line("loop:");
            c.open("loop");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ {}, %loop ]",
                acc_ll, init_lit, val
            ));
            let (xv, xty, x2v) = elem_loads(&mut c, base, &start_v, &spec.base_elems, mdl);
            c.scalar.insert(el.clone(), (xv.clone(), xty));
            if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                c.scalar.insert(name.clone(), (reg.clone(), *ty));
            }
            c.scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            c.lines.extend(scratch.lines);
            carried = val;
            carried_block = "%loop";
            c.line("  %i.next = add i64 %i, 1");
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, {}", t_cond, count_v));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
        // Single-block pipeline combine: no stages, a binop combine can't fork.
        HReduce::Pipeline { combine, .. } if maps.is_empty() && filters.is_empty() => {
            let _ = combine;
            c.line("");
            c.line("loop:");
            c.open("loop");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ %acc.next, %loop ]",
                acc_ll, init_lit
            ));
            let (xv, xty, x2v) = elem_loads(&mut c, base, &start_v, &spec.base_elems, mdl);
            c.scalar.insert(el.clone(), (xv.clone(), xty));
            if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                c.scalar.insert(name.clone(), (reg.clone(), *ty));
            }
            c.scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let combined = combine_step(*combine, &xv, "%acc", spec.acc_ty, &mut c);
            c.line(&format!("  %acc.next = {}", combined));
            carried = "%acc.next".to_string();
            carried_block = "%loop";
            c.line("  %i.next = add i64 %i, 1");
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, {}", t_cond, count_v));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
        // Structured apply/skip/merge: any maps, filters, or control flow.
        _ => {
            c.line("");
            c.line("loop:");
            c.open("loop");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %merge ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ %acc.merge, %merge ]",
                acc_ll, init_lit
            ));
            let (xv, xty, x2v) = elem_loads(&mut c, base, &start_v, &spec.base_elems, mdl);

            if !filters.is_empty() {
                for fl in filters {
                    c.scalar.insert(fl.el.clone(), (xv.clone(), fl.el_ty));
                    if let (Some(name), Some((reg, ty))) = (&fl.el2, &x2v) {
                        c.scalar.insert(name.clone(), (reg.clone(), *ty));
                    }
                    let pv = emit_bool(&fl.body, &mut c, mdl, sigs);
                    let t_keep = c.temp();
                    c.line(&format!("  {} = icmp ne i1 {}, 0", t_keep, pv));
                    c.line(&format!("  br i1 {}, label %apply, label %skip", t_keep));
                }
            } else {
                c.line("  br label %apply");
            }

            c.line("");
            c.line("apply:");
            c.open("apply");
            let mut val = xv.clone();
            let mut val_ty = xty;
            for m in maps.iter() {
                c.scalar.insert(m.el.clone(), (val.clone(), m.el_ty));
                if let (Some(name), Some((reg, ty))) = (&m.el2, &x2v) {
                    c.scalar.insert(name.clone(), (reg.clone(), *ty));
                }
                val = emit_typed(&m.body, m.res_ty, &mut c, mdl, sigs);
                val_ty = m.res_ty;
            }
            c.scalar
                .insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let r = match &spec.reduce {
                HReduce::Closure { body, .. } => {
                    c.scalar.insert(el.clone(), (val.clone(), val_ty));
                    if let (Some(name), Some((reg, ty))) = (&el2, &x2v) {
                        c.scalar.insert(name.clone(), (reg.clone(), *ty));
                    }
                    emit_typed(body, spec.acc_ty, &mut c, mdl, sigs)
                }
                HReduce::Pipeline { combine, .. } => {
                    let t = c.temp();
                    let combined =
                        combine_step(*combine, &val, "%acc", spec.acc_ty, &mut c);
                    c.line(&format!("  {} = {}", t, combined));
                    t
                }
            };
            let r = pin_value(acc_ll, &r, &mut c);
            let apply_pred = c.cur.clone();
            c.line("  br label %merge");

            c.line("");
            c.line("skip:");
            c.open("skip");
            c.line("  br label %merge");

            c.line("");
            c.line("merge:");
            c.open("merge");
            c.line(&format!(
                "  %acc.merge = phi {} [ {}, %{} ], [ %acc, %skip ]",
                acc_ll, r, apply_pred
            ));
            c.line("  %i.next = add i64 %i, 1");
            carried = "%acc.merge".to_string();
            carried_block = "%merge";
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, {}", t_cond, count_v));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
    }

    c.line("");
    c.line("ret:");
    c.open("ret");
    let t_ret = c.temp();
    c.line(&format!(
        "  {} = phi {} [ {}, %entry ], [ {}, {} ]",
        t_ret, acc_ll, init_lit, carried, carried_block
    ));
    c.line(&format!("  ret {} {}", acc_ll, t_ret));

    let mut lines = FnCtx::with_prelude(c.lines, &c.prelude);
    if mdl.fast {
        // Tag the loop backedge: LLVM then auto-vectorizes the pure fused
        // loop (spec: auto-SIMD).
        for line in lines.iter_mut() {
            if line.starts_with("  br i1 ") && line.ends_with("label %loop, label %ret") {
                *line = format!("{}, !llvm.loop !0", line);
            }
        }
    }
    let mut text = format!("define {} @{}(", acc_ll, spec.helper);
    text.push_str(&params.join(", "));
    text.push_str(") {\n");
    text.push_str(&lines.join("\n"));
    text.push('\n');
    text.push_str("}\n");
    text
}

pub(crate) fn loop_init(r: &HReduce) -> f64 {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => match init {
            HExpr::ConstF64(v) => *v,
            HExpr::ConstI64(v) => *v as f64,
            HExpr::ConvF64(inner) => match &**inner {
                HExpr::ConstI64(v) => *v as f64,
                _ => 0.0,
            },
            _ => 0.0,
        },
    }
}

pub(crate) fn combine_step(op: BinOp, val: &str, acc: &str, ty: Ty, _c: &mut FnCtx) -> String {
    let (fop, iop) = match op {
        BinOp::Add => ("fadd", "add"),
        BinOp::Sub => ("fsub", "sub"),
        BinOp::Mul => ("fmul", "mul"),
        BinOp::Div => ("fdiv", "sdiv"),
    };
    let (op, sigil) = if ty == Ty::F64 { (fop, "double") } else { (iop, "i64") };
    format!("{} {} {}, {}", op, sigil, acc, val)
}

// ---------------------------------------------------------------------------
// Plain (non-loop) function emission
// ---------------------------------------------------------------------------
