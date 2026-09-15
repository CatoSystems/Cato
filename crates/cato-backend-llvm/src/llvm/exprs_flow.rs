use super::*;

pub(crate) fn emit_float(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstF64(v) => llvm_f64(*v),
        HExpr::Get { ty: Ty::F64, .. } => emit_get(e, c, mdl, sigs),
        HExpr::Clone(inner) => emit_float(inner, c, mdl, sigs),
        HExpr::Let(binds, body, frees) => {
            emit_let(binds, c, mdl, sigs);
            let v = emit_float(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
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
        HExpr::Bin(op, a, b) => {
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
        HExpr::ICast(inner, target) if *target == Ty::F64 || *target == Ty::F32 => {
            let src = static_int_width(inner, c, sigs);
            let iv = emit_typed(inner, src, c, mdl, sigs);
            emit_cast(iv, src, *target, c)
        }
        HExpr::ConvF64(v) => {
            let iv = emit_int(v, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = sitofp i64 {} to double", t, iv));
            t
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
            emit_let(binds, c, mdl, sigs);
            let v = emit_float(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
        }
        HExpr::Seq(vs) => {
            for v in vs {
                emit_stmt(v, c, mdl, sigs);
            }
            "0.0".to_string()
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
