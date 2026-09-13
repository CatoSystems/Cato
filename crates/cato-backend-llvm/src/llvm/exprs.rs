use super::*;

/// Emit a widening/narrowing/conversion cast between scalar widths:
/// same-bits ints reinterpret for free, narrowing truncates, signed
/// widening sexts, unsigned widening zexts, and the float matrix maps
/// to sitofp/uitofp/fptosi/fptoui/fptrunc/fpext.
pub(crate) fn emit_cast(r: String, src: Ty, target: Ty, c: &mut FnCtx) -> String {
    if src == target {
        return r;
    }
    let t = c.temp();
    let sll = if src.is_float() { Mdl::ll_scalar(src) } else { Mdl::ll_scalar(src) };
    let dll = Mdl::ll_scalar(target);
    if src.is_int() && target.is_int() {
        let (sb, db) = (src.int_bits(), target.int_bits());
        if sb == db {
            return r; // reinterpret: no instruction
        }
        if sb > db {
            c.line(&format!("  {} = trunc {} {} to {}", t, sll, r, dll));
        } else if src.is_uint() {
            c.line(&format!("  {} = zext {} {} to {}", t, sll, r, dll));
        } else {
            c.line(&format!("  {} = sext {} {} to {}", t, sll, r, dll));
        }
    } else if src.is_int() && target.is_float() {
        let op = if src.is_uint() { "uitofp" } else { "sitofp" };
        c.line(&format!("  {} = {} {} {} to {}", t, op, sll, r, dll));
    } else if src.is_float() && target.is_int() {
        let op = if target.is_uint() { "fptoui" } else { "fptosi" };
        c.line(&format!("  {} = {} {} {} to {}", t, op, sll, r, dll));
    } else {
        // float <-> float
        let op = if target == Ty::F32 { "fptrunc" } else { "fpext" };
        c.line(&format!("  {} = {} {} {} to {}", t, op, sll, r, dll));
    }
    t
}

pub(crate) fn emit_bool(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::CmpW(op, a, b, ty) => {
            let av = emit_typed(a, *ty, c, mdl, sigs);
            let bv = emit_typed(b, *ty, c, mdl, sigs);
            if ty.is_float() {
                let ll = Mdl::ll_scalar(*ty);
                let op = match op {
                    CmpOp::Eq => "oeq",
                    CmpOp::Ne => "une",
                    CmpOp::Lt => "olt",
                    CmpOp::Gt => "ogt",
                    CmpOp::Le => "ole",
                    CmpOp::Ge => "oge",
                };
                let t = c.temp();
                c.line(&format!("  {} = fcmp {} {} {}, {}", t, op, ll, av, bv));
                return t;
            }
            let ll = Mdl::ll_scalar(*ty);
            let op = match op {
                CmpOp::Eq => "eq",
                CmpOp::Ne => "ne",
                // Unsigned widths use the `ult` family.
                CmpOp::Lt if ty.is_uint() => "ult",
                CmpOp::Gt if ty.is_uint() => "ugt",
                CmpOp::Le if ty.is_uint() => "ule",
                CmpOp::Ge if ty.is_uint() => "uge",
                CmpOp::Lt => "slt",
                CmpOp::Gt => "sgt",
                CmpOp::Le => "sle",
                CmpOp::Ge => "sge",
            };
            let t = c.temp();
            c.line(&format!("  {} = icmp {} {} {}, {}", t, op, ll, av, bv));
            t
        }
        HExpr::StrCmp(eq, a, b) => {
            let av = emit_str_val(a, c, mdl, sigs);
            let bv = emit_str_val(b, c, mdl, sigs);
            mdl.declare("declare i32 @strcmp(ptr, ptr)".to_string());
            let t = c.temp();
            c.line(&format!("  {} = call i32 @strcmp(ptr {}, ptr {})", t, av, bv));
            let z = c.temp();
            c.line(&format!(
                "  {} = icmp {} i32 {}, 0",
                z,
                if *eq { "eq" } else { "ne" },
                t
            ));
            z
        }
        HExpr::SetContains { .. } => emit_set_contains(e, c, mdl, sigs),
        HExpr::Cmp(op, a, b, is_f64) => {
            if *is_f64 {
                let av = emit_float(a, c, mdl, sigs);
                let bv = emit_float(b, c, mdl, sigs);
                let op = match op {
                    CmpOp::Eq => "oeq",
                    CmpOp::Ne => "one",
                    CmpOp::Lt => "olt",
                    CmpOp::Gt => "ogt",
                    CmpOp::Le => "ole",
                    CmpOp::Ge => "oge",
                };
                let t = c.temp();
                c.line(&format!("  {} = fcmp {} double {}, {}", t, op, av, bv));
                t
            } else {
                let av = emit_int(a, c, mdl, sigs);
                let bv = emit_int(b, c, mdl, sigs);
                let op = match op {
                    CmpOp::Eq => "eq",
                    CmpOp::Ne => "ne",
                    CmpOp::Lt => "slt",
                    CmpOp::Gt => "sgt",
                    CmpOp::Le => "sle",
                    CmpOp::Ge => "sge",
                };
                let t = c.temp();
                c.line(&format!("  {} = icmp {} i64 {}, {}", t, op, av, bv));
                t
            }
        }
        HExpr::And(a, b) => {
            let la = emit_bool(a, c, mdl, sigs);
            let parent = c.cur.clone();
            let rhs = c.block_name();
            let merged = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", la, rhs, merged));
            let lbl = c.open(&rhs);
            c.line(&format!("{}:", lbl));
            let rb = emit_bool(b, c, mdl, sigs);
            let bp = c.cur.clone();
            c.line(&format!("  br label %{}", merged));
            let lbl = c.open(&merged);
            c.line(&format!("{}:", lbl));
            let res = c.temp();
            c.line(&format!(
                "  {} = phi i1 [ false, %{} ], [ {}, %{} ]",
                res, parent, rb, bp
            ));
            res
        }
        HExpr::Or(a, b) => {
            let la = emit_bool(a, c, mdl, sigs);
            let parent = c.cur.clone();
            let rhs = c.block_name();
            let merged = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", la, merged, rhs));
            let lbl = c.open(&rhs);
            c.line(&format!("{}:", lbl));
            let rb = emit_bool(b, c, mdl, sigs);
            let bp = c.cur.clone();
            c.line(&format!("  br label %{}", merged));
            let lbl = c.open(&merged);
            c.line(&format!("{}:", lbl));
            let res = c.temp();
            c.line(&format!(
                "  {} = phi i1 [ true, %{} ], [ {}, %{} ]",
                res, parent, rb, bp
            ));
            res
        }
        HExpr::Not(v) => {
            let iv = emit_bool(v, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = xor i1 {}, true", t, iv));
            t
        }
        HExpr::Var(v) => {
            let (r, _) = c.scalar.get(v).unwrap_or_else(|| panic!("codegen: unknown bool var {}", v));
            r.clone()
        }
        HExpr::If(cond, then, els, Ty::Bool) => emit_if_node(cond, then, els, Ty::Bool, c, mdl, sigs),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Call(name, args) if name == "even?" => {
            let v = emit_int(&args[0], c, mdl, sigs);
            let p = c.temp();
            c.line(&format!("  {} = and i64 {}, 1", p, v));
            let t = c.temp();
            c.line(&format!("  {} = icmp eq i64 {}, 0", t, p));
            t
        }
        other => panic!("codegen: expected a boolean expression, got {:?}", other),
    }
}

pub(crate) fn emit_if_node(
    cond: &HExpr,
    then: &HExpr,
    els: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let lc = emit_bool(cond, c, mdl, sigs);
    let bt = c.block_name();
    let be = c.block_name();
    let bm = c.block_name();
    c.line(&format!("  br i1 {}, label %{}, label %{}", lc, bt, be));
    let lbl = c.open(&bt);
    c.line(&format!("{}:", lbl));
    let tv = emit_typed(then, ty, c, mdl, sigs);
    let tp = c.cur.clone();
    c.line(&format!("  br label %{}", bm));
    let lbl = c.open(&be);
    c.line(&format!("{}:", lbl));
    let ev = emit_typed(els, ty, c, mdl, sigs);
    let ep = c.cur.clone();
    c.line(&format!("  br label %{}", bm));
    let lbl = c.open(&bm);
    c.line(&format!("{}:", lbl));
    let res = c.temp();
    let lty = match ty {
        Ty::F64 => "double".to_string(),
        Ty::Bool => "i1".to_string(),
        Ty::Adt(_) => "ptr".to_string(),
        _ => "i64".to_string(),
    };
    c.line(&format!(
        "  {} = phi {} [ {}, %{} ], [ {}, %{} ]",
        res, lty, tv, tp, ev, ep
    ));
    res
}

// ---------------------------------------------------------------------------
// ADTs: constructors and match
// ---------------------------------------------------------------------------

/// Which ADT an expression of ADT type denotes (needed to read variant tags).
pub(crate) fn emit_int(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstI64(v) => v.to_string(),
        HExpr::Reduce(_) => panic!("codegen: Reduce in i64 context (rewrite bug)"),
        HExpr::Var(v) => {
            if let Some((r, _)) = c.scalar.get(v) {
                r.clone()
            } else {
                // vector var used in length context
                let (_, l, _) = c.vecs.get(v).expect("known var");
                l.clone()
            }
        }
        HExpr::Bin(op, a, b) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let op = match op {
                BinOp::Add => "add",
                BinOp::Sub => "sub",
                BinOp::Mul => "mul",
                BinOp::Div => "sdiv",
            };
            let t = c.temp();
            c.line(&format!("  {} = {} i64 {}, {}", t, op, av, bv));
            t
        }
        HExpr::ConvF64(_) => panic!("codegen: ConvF64 in i64 context"),
        HExpr::Call(name, args) if name == "print-i64" => {
            // Side-effecting call inside a loop body (e.g. doseq).
            let v = emit_int(&args[0], c, mdl, sigs);
            let w = static_int_width(&args[0], c, sigs);
            let v64 = if w == Ty::I64 {
                v
            } else if w == Ty::Bool {
                let t = c.temp();
                c.line(&format!("  {} = zext i1 {} to i64", t, v));
                t
            } else if w.is_float() {
                let t = c.temp();
                c.line(&format!(
                    "  {} = fptosi {} {} to i64",
                    t,
                    Mdl::ll_scalar(w),
                    v
                ));
                t
            } else if w.is_uint() {
                let t = c.temp();
                c.line(&format!("  {} = zext {} {} to i64", t, Mdl::ll_scalar(w), v));
                t
            } else {
                let t = c.temp();
                c.line(&format!("  {} = sext {} {} to i64", t, Mdl::ll_scalar(w), v));
                t
            };
            c.line(&format!("  call void @cato_print_i64(i64 {})", v64));
            "0".to_string()
        }
        HExpr::Call(name, args) if name == "print-string" => {
            let v = emit_str_val(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_string(ptr {})", v));
            "0".to_string()
        }
        HExpr::Count(v) => match &**v {
            HExpr::Var(s) => {
                if c.grow.contains_key(s) {
                    let (_, l, _) = grow_val(c, s);
                    l
                } else if c.growm.contains_key(s) {
                    let (_, l) = {
                        let slot = c.growm.get(s).unwrap().0.clone();
                        let lf = c.temp();
                        c.line(&format!(
                            "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 1",
                            lf, slot
                        ));
                        let l = c.temp();
                        c.line(&format!("  {} = load i64, ptr {}, align 8", l, lf));
                        (slot, l)
                    };
                    l
                } else if let Some((_, l, _)) = c.vecs.get(s) {
                    l.clone()
                } else {
                    let (r, ty) = c
                        .scalar
                        .get(s)
                        .cloned()
                        .unwrap_or_else(|| panic!("codegen: count of unknown var {}", s));
                    if ty == Ty::Str {
                        mdl.declare("declare i64 @strlen(ptr)".to_string());
                        let t = c.temp();
                        c.line(&format!("  {} = call i64 @strlen(ptr {})", t, r));
                        t
                    } else {
                        panic!("codegen: count of non-vector/non-string {}", s)
                    }
                }
            }
            HExpr::StrLit(_) => {
                let p = emit_str_val(v, c, mdl, sigs);
                mdl.declare("declare i64 @strlen(ptr)".to_string());
                let t = c.temp();
                c.line(&format!("  {} = call i64 @strlen(ptr {})", t, p));
                t
            }
            HExpr::VecLit(_) => {
                let (_, l, _) = vec_arg(v, c, mdl);
                l
            }
            other => panic!("codegen: count of {:?}", other),
        },
        HExpr::Call(name, args) => {
            let v = call_user(name, args, c, mdl, sigs);
            // A bool-returning callee used in i64 position widens to 0/1.
            let is_bool = sigs
                .get(name)
                .map(|(r, _)| *r == Ty::Bool)
                .unwrap_or(false);
            if is_bool {
                let t = c.temp();
                c.line(&format!("  {} = zext i1 {} to i64", t, v));
                t
            } else {
                v
            }
        }
        HExpr::Let(binds, body, frees) => {
            emit_let(binds, c, mdl, sigs);
            let v = emit_int(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
        }
        HExpr::Seq(vs) => {
            for v in vs {
                emit_stmt(v, c, mdl, sigs);
            }
            "0".to_string()
        }
        HExpr::If(cond, then, els, t) => emit_if_node(cond, then, els, *t, c, mdl, sigs),
        HExpr::Abs(e, Ty::I32) => {
            let v = emit_int(e, c, mdl, sigs);
            let neg = c.temp();
            c.line(&format!("  {} = sub i32 0, {}", neg, v));
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i32 {}, 0", lt, v));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i32 {}, i32 {}", t, lt, neg, v));
            t
        }
        HExpr::Min(a, b, Ty::I32) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i32 {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i32 {}, i32 {}", t, lt, av, bv));
            t
        }
        HExpr::Max(a, b, Ty::I32) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = icmp sgt i32 {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i32 {}, i32 {}", t, lt, av, bv));
            t
        }
        HExpr::Abs(e, _) => {
            let v = emit_int(e, c, mdl, sigs);
            let neg = c.temp();
            c.line(&format!("  {} = sub i64 0, {}", neg, v));
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, 0", lt, v));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, neg, v));
            t
        }
        HExpr::Min(a, b, _) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = icmp slt i64 {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, av, bv));
            t
        }
        HExpr::Max(a, b, _) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let lt = c.temp();
            c.line(&format!("  {} = icmp sgt i64 {}, {}", lt, av, bv));
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, av, bv));
            t
        }
        HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Cmp(..)
        | HExpr::StrCmp(..) | HExpr::CmpW(..) | HExpr::SetContains { .. } => {
            // Bool value in i64 position: widen 0/1 (e.g. print-i64 of a test).
            let b = emit_bool(e, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = zext i1 {} to i64", t, b));
            t
        }
        HExpr::ConstF64(v) => panic!("codegen: ConstF64 {} in i64 context", v),
        HExpr::VecLit(_) => panic!("codegen: vec literal in i64 context"),
        HExpr::Match { .. } => emit_match_node(e, c, mdl, sigs),
        HExpr::Ctor { .. } => {
            emit_adt(e, c, mdl, sigs);
            "0".to_string()
        }
        HExpr::Get { ty: Ty::I64, .. } => emit_get(e, c, mdl, sigs),
        HExpr::Clone(inner) => emit_int(inner, c, mdl, sigs),
        HExpr::DropStmt(_) => "0".to_string(),
        HExpr::StrLit(_) => panic!("codegen: string literal in i64 context"),
        HExpr::Conj { .. } | HExpr::Subvec { .. } | HExpr::For { .. } => panic!(
            "codegen: heap vector value must be bound with let"
        ),
        HExpr::StrCat(_) => panic!("codegen: str result must be bound with let"),
        HExpr::MapLit(_) | HExpr::MapAssoc { .. } => panic!(
            "codegen: map value must be bound with let"
        ),
        HExpr::MapGet { .. } => emit_map_get(e, c, mdl, sigs),
        HExpr::BinW(op, a, b, ty) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            let op = match op {
                BinOp::Add => "add",
                BinOp::Sub => "sub",
                BinOp::Mul => "mul",
                BinOp::Div if ty.is_uint() => "udiv",
                BinOp::Div => "sdiv",
            };
            let ll = Mdl::ll_scalar(*ty);
            let t = c.temp();
            c.line(&format!("  {} = {} {} {}, {}", t, op, ll, av, bv));
            t
        }
        HExpr::ICast(inner, target) => {
            let src = static_int_width(inner, c, sigs);
            let iv = emit_typed(inner, src, c, mdl, sigs);
            emit_cast(iv, src, *target, c)
        }
        HExpr::BinF(..) => panic!("codegen: f32 arithmetic in i64 context"),
        HExpr::SetLit(_)
        | HExpr::SetConj { .. }
        | HExpr::SetDisj { .. }
        | HExpr::Subs { .. }
        | HExpr::Split { .. }
        | HExpr::Join { .. } => {
            panic!("codegen: heap value must be bound with let")
        }
        HExpr::Get { ty, .. } => panic!(
            "codegen: get returning {:?} in i64 context (expected F64 context)",
            ty
        ),
    }
    .to_string()
}

