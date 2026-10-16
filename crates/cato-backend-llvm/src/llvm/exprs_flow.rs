use super::*;

pub(crate) fn emit_float(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstF64(v) => llvm_f64(*v),
        HExpr::Get { ty: Ty::F64, .. } => emit_get(e, c, mdl, sigs),
        HExpr::Clone(inner) => emit_float(inner, c, mdl, sigs),
        HExpr::Loop { .. } => emit_loop_node(e, c, mdl, sigs),
        HExpr::Let(binds, body, frees) => {
            emit_let_scoped(binds, frees, c, mdl, sigs, |c, mdl| {
                emit_float(body, c, mdl, sigs)
            })
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_float(vs.last().unwrap(), c, mdl, sigs)
        }
        HExpr::Var(v) => {
            let (r, _) = c.scalar.get(v).expect("known f64 var");
            r.clone()
        }
        HExpr::BinF(op, a, b) => {
            let av = emit_typed(a, Ty::F32, c, mdl, sigs);
            let bv = emit_typed(b, Ty::F32, c, mdl, sigs);
            let op = match op {
                BinOp::Add => "fadd",
                BinOp::Sub => "fsub",
                BinOp::Mul => "fmul",
                BinOp::Div => "fdiv",
            };
            let t = c.temp();
            c.line(&format!("  {} = {} float {}, {}", t, op, av, bv));
            t
        }
        HExpr::BinW(op, a, b, Ty::F64) => {
            let av = emit_float(a, c, mdl, sigs);
            let bv = emit_float(b, c, mdl, sigs);
            let op = match op {
                BinOp::Add => "fadd",
                BinOp::Sub => "fsub",
                BinOp::Mul => "fmul",
                BinOp::Div => "fdiv",
            };
            let t = c.temp();
            c.line(&format!("  {} = {} double {}, {}", t, op, av, bv));
            t
        }
        HExpr::ICast(inner, from, target) if *target == Ty::F64 || *target == Ty::F32 => {
            let iv = emit_typed(inner, *from, c, mdl, sigs);
            emit_cast(iv, *from, *target, c)
        }
        HExpr::Call(name, args) => {
            if name == "print-f64" {
                let v = emit_float(&args[0], c, mdl, sigs);
                // f32 values widen to double for printing.
                let w = static_int_width(&args[0], c, sigs);
                let vd = if w == Ty::F32 {
                    let t = c.temp();
                    c.line(&format!("  {} = fpext float {} to double", t, v));
                    t
                } else {
                    v
                };
                c.line(&format!("  call void @cato_print_f64(double {})", vd));
                "0.0".to_string()
            } else {
                call_user(name, args, c, mdl, sigs)
            }
        }
        HExpr::Let(binds, body, frees) => {
            emit_let_scoped(binds, frees, c, mdl, sigs, |c, mdl| {
                emit_float(body, c, mdl, sigs)
            })
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_float(vs.last().unwrap(), c, mdl, sigs)
        }
        HExpr::ConstI64(v) => {
            let t = c.temp();
            c.line(&format!("  {} = sitofp i64 {} to double", t, v));
            t
        }
        HExpr::If(cond, then, els, t) => emit_if_node(cond, then, els, *t, c, mdl, sigs),
        HExpr::Abs(e, _) => {
            let v = emit_float(e, c, mdl, sigs);
            let neg = c.temp();
            c.line(&format!("  {} = fneg double {}", neg, v));
            let lt = c.temp();
            c.line(&format!("  {} = fcmp olt double {}, 0.000000e+00", lt, v));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, neg, v));
            t
        }
        HExpr::Min(a, b, _) => {
            let av = emit_float(a, c, mdl, sigs);
            let bv = emit_float(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = fcmp olt double {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, av, bv));
            t
        }
        HExpr::Max(a, b, _) => {
            let av = emit_float(a, c, mdl, sigs);
            let bv = emit_float(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = fcmp ogt double {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, av, bv));
            t
        }
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Ctor { .. } => {
            emit_adt(e, c, mdl, sigs);
            "0.0".to_string()
        }
        other => panic!("codegen: unsupported node in f64 context: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// S2: loop / recur — general while-loop codegen
// ---------------------------------------------------------------------------

/// The LLVM type of a loop variable slot. Beyond the Copy scalars/str, S8
/// admits boxed ADTs (the slot is the owning pointer) and collection
/// headers (a %VecI64 {ptr,len,cap} the loop's reads see live). Inline ADT
/// and closure variables stay unsupported.
fn loop_slot_ty(mdl: &Mdl, ty: Ty) -> &'static str {
    match ty {
        Ty::F64 => "double",
        Ty::F32 => "float",
        Ty::Bool => "i1",
        Ty::Str => "ptr",
        t if t.is_int() => Mdl::ll_scalar(t),
        Ty::Adt(i) if mdl.adt_boxed(i) => "ptr",
        Ty::Vec(_) | Ty::Map | Ty::Set => "%VecI64",
        o => panic!("codegen: loop variable of unsupported type {:?}", o),
    }
}

/// `(loop [x :- ty init ...] body)`: one alloca per variable, inits stored
/// before the head; each iteration loads fresh registers at the head;
/// `recur` stores its arguments and branches back; the completing path
/// branches to the merge block.
pub(crate) fn emit_loop_node(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::Loop { binds, body, ty } = e else {
        panic!("codegen: emit_loop_node on non-loop")
    };
    // Slots hoisted to the entry prelude: the loop may execute any number
    // of times, and allocas in loops would stack up.
    let mut slots: Vec<(String, String, Ty)> = Vec::new();
    for (name, ty, init) in binds {
        // A variable seeded from another growable binding shares its buffer:
        // two live headers over one buffer must never both append in place.
        if let HExpr::Var(src) = init {
            if slots.iter().any(|(n, _, _)| n == src) || c.grow.contains_key(src) {
                c.loop_alias.insert(name.clone());
            }
        }
        let llt = loop_slot_ty(mdl, *ty);
        let slot = c.temp();
        c.prelude
            .push(format!("  {} = alloca {}, align 8", slot, llt));
        let v = emit_typed(init, *ty, c, mdl, sigs);
        c.line(&format!("  store {} {}, ptr {}, align 8", llt, v, slot));
        slots.push((name.clone(), slot, *ty));
    }
    // A fresh loop body starts with no in-flight recur append.
    let saved_fast = c.recur_fast.take();
    let head = c.block_name();
    c.line(&format!("  br label %{}", head));
    let l = c.open(&head);
    c.line(&format!("{}:", l));
    // Each iteration reads the variables into fresh registers. The
    // bindings end with the loop: an outer name a loop variable shadows
    // must be visible again after the merge. Collection variables skip
    // the load: they register their slot, so every read sees the live
    // (possibly in-place grown) header.
    let names: Vec<String> = slots.iter().map(|(n, _, _)| n.clone()).collect();
    let scope = c.save_binds(&names);
    for (name, slot, ty) in &slots {
        match ty {
            Ty::Vec(elem) => {
                c.grow.insert(name.clone(), (slot.clone(), *elem));
            }
            Ty::Map | Ty::Set => {
                c.growm.insert(name.clone(), (slot.clone(), *ty));
            }
            _ => {
                let r = c.temp();
                c.line(&format!(
                    "  {} = load {}, ptr {}, align 8",
                    r,
                    loop_slot_ty(mdl, *ty),
                    slot
                ));
                c.scalar.insert(name.clone(), (r, *ty));
            }
        }
    }
    let saved = c.loop_frame.take();
    c.loop_frame = Some((slots, head));
    let completing = emit_loop_tail(body, *ty, c, mdl, sigs);
    c.loop_frame = saved;
    c.recur_fast = saved_fast;
    c.restore_binds(scope);
    let after = c.block_name();
    match completing {
        Some(v) => {
            c.line(&format!("  br label %{}", after));
            let l = c.open(&after);
            c.line(&format!("{}:", l));
            // Single completing path chain: the value dominates the merge.
            v
        }
        None => {
            // Every path recurs: the merge is unreachable.
            let l = c.open(&after);
            c.line(&format!("{}:", l));
            c.line("  unreachable");
            "undef".to_string()
        }
    }
}

/// Emit a loop body in tail position. `Some(register)` = the path completes
/// with this value (no terminator emitted; the caller branches to its
/// merge). `None` = every path in this expression already branched back to
/// the loop head.
fn emit_loop_tail(
    e: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> Option<String> {
    let (slots, head) = c.loop_frame.clone().expect("codegen: loop tail outside loop");
    match e {
        HExpr::Recur(args) => {
            // `(recur ... (conj v x) ...)` where the i-th argument is exactly
            // a conj of the i-th loop variable *is* the loop's accumulator
            // update: the value the slot holds is replaced for the next
            // iteration, so the append may grow the buffer in place. Every
            // header read in this iteration still sees the pre-append length,
            // so the old value is exactly what a persistent copy would
            // report. A second conj of the same variable among the arguments
            // would interleave two appends at one index — keep copying then.
            let mut conj_counts: HashMap<&str, usize> = HashMap::new();
            for a in args.iter() {
                if let HExpr::Conj { v, .. } = a {
                    if let HExpr::Var(n) = &**v {
                        *conj_counts.entry(n.as_str()).or_insert(0) += 1;
                    }
                }
            }
            for (a, (name, slot, vty)) in args.iter().zip(slots.iter()) {
                let self_update = match a {
                    HExpr::Conj { v, .. } => matches!(&**v, HExpr::Var(n) if n == name),
                    _ => false,
                } && conj_counts.get(name.as_str()) == Some(&1)
                    && !c.loop_alias.contains(name);
                if self_update {
                    c.recur_fast = Some(name.clone());
                }
                let v = emit_typed(a, *vty, c, mdl, sigs);
                if self_update {
                    c.recur_fast = None;
                }
                c.line(&format!(
                    "  store {} {}, ptr {}, align 8",
                    loop_slot_ty(mdl, *vty),
                    v,
                    slot
                ));
            }
            c.line(&format!("  br label %{}", head));
            None
        }
        HExpr::If(cond, then, els, ity) => {
            let lc = emit_bool(cond, c, mdl, sigs);
            let bt = c.block_name();
            let be = c.block_name();
            let bm = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", lc, bt, be));
            let l = c.open(&bt);
            c.line(&format!("{}:", l));
            let tv = emit_loop_tail(then, *ity, c, mdl, sigs);
            let tp = if tv.is_some() {
                c.line(&format!("  br label %{}", bm));
                Some(c.cur.clone())
            } else {
                None
            };
            let l = c.open(&be);
            c.line(&format!("{}:", l));
            let ev = emit_loop_tail(els, *ity, c, mdl, sigs);
            let ep = if ev.is_some() {
                c.line(&format!("  br label %{}", bm));
                Some(c.cur.clone())
            } else {
                None
            };
            let l = c.open(&bm);
            c.line(&format!("{}:", l));
            match (tv, ev) {
                (Some(tv), Some(ev)) => {
                    // Boxed ADTs phi as ptrs, collections as %VecI64 —
                    // the same slot mapping the loop variables use.
                    let lty = loop_slot_ty(mdl, *ity);
                    let r = c.temp();
                    c.line(&format!(
                        "  {} = phi {} [ {}, %{} ], [ {}, %{} ]",
                        r, lty, tv, tp.unwrap(), ev, ep.unwrap()
                    ));
                    Some(r)
                }
                (Some(v), None) | (None, Some(v)) => Some(v),
                (None, None) => {
                    c.line("  unreachable");
                    None
                }
            }
        }
        HExpr::Let(binds, body, frees) => {
            let names: Vec<String> = binds.iter().map(|(n, _, _)| n.clone()).collect();
            let scope = c.save_binds(&names);
            emit_let(binds, c, mdl, sigs);
            let v = emit_loop_tail(body, ty, c, mdl, sigs);
            // Scope-exit frees run only on the completing path — a `recur`
            // leaves this scope and must not release anything.
            if v.is_some() {
                emit_let_frees(frees, c, mdl);
            }
            c.restore_binds(scope);
            v
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_loop_tail(vs.last().unwrap(), ty, c, mdl, sigs)
        }
        // A match on the tail path (e.g. the reader's frame-pop dispatch):
        // arms branch via emit_loop_tail, so recur-only arms contribute no
        // phi input and the loop continues through them.
        HExpr::Match { .. } => emit_match_in_loop(e, c, mdl, sigs, &emit_loop_tail),
        other => Some(emit_typed(other, ty, c, mdl, sigs)),
    }
}
