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
        HExpr::ConstB(v) => (if *v { "true" } else { "false" }).to_string(),
        HExpr::Loop { .. } => emit_loop_node(e, c, mdl, sigs),
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
        HExpr::StrOrd(op, a, b) => {
            // Byte-wise order via the strcmp sign: the i32 result compares
            // against zero (signed).
            let av = emit_str_val(a, c, mdl, sigs);
            let bv = emit_str_val(b, c, mdl, sigs);
            mdl.declare("declare i32 @strcmp(ptr, ptr)".to_string());
            let t = c.temp();
            c.line(&format!("  {} = call i32 @strcmp(ptr {}, ptr {})", t, av, bv));
            let cc = match op {
                CmpOp::Lt => "slt",
                CmpOp::Gt => "sgt",
                CmpOp::Le => "sle",
                CmpOp::Ge => "sge",
                CmpOp::Eq | CmpOp::Ne => unreachable!("StrOrd carries an ordering op"),
            };
            let z = c.temp();
            c.line(&format!("  {} = icmp {} i32 {}, 0", z, cc, t));
            z
        }
        HExpr::SetContains { .. } => emit_set_contains(e, c, mdl, sigs),
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
        HExpr::Let(binds, body, frees) => {
            emit_let_scoped(binds, frees, c, mdl, sigs, |c, mdl| emit_bool(body, c, mdl, sigs))
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_bool(vs.last().unwrap(), c, mdl, sigs)
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
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
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
        Ty::Str => "ptr".to_string(),
        Ty::F32 => "float".to_string(),
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
            let found = c.scalar.get(v).map(|(r, ty)| (r.clone(), *ty));
            if let Some((r, ty)) = found {
                if ty == Ty::Bool {
                    // Bool binding in i64 position: widen 0/1.
                    let t = c.temp();
                    c.line(&format!("  {} = zext i1 {} to i64", t, r));
                    t
                } else {
                    r
                }
            } else {
                // vector var used in length context
                let (_, l, _) = c.vecs.get(v).expect("known var");
                l.clone()
            }
        }
        HExpr::Call(name, args) if name == "print-f64" => {
            // print method body in i64 context (print returns 0).
            let v = emit_float(&args[0], c, mdl, sigs);
            let w = static_int_width(&args[0], c, sigs);
            let vd = if w == Ty::F32 {
                let t = c.temp();
                c.line(&format!("  {} = fpext float {} to double", t, v));
                t
            } else {
                v
            };
            c.line(&format!("  call void @cato_print_f64(double {})", vd));
            "0".to_string()
        }
        HExpr::Call(name, args) if name == "print-i64" => {
            // Side-effecting call inside a loop body (e.g. doseq).
            let v = emit_int(&args[0], c, mdl, sigs);
            let w = static_int_width(&args[0], c, sigs);
            let v64 = if w == Ty::I64 || w == Ty::U64 || w == Ty::Usize {
                // Same bits: the i64 register already holds the value.
                v
            } else if w == Ty::Bool {
                // emit_int already widened the bool read to 0/1.
                v
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
                let (_, l, _) = vec_arg(v, c, mdl, sigs);
                l
            }
            HExpr::Clone(inner) => {
                // Count of a clone: the copy's length equals the
                // original's, so no copy is emitted. A vector clone
                // reads the original's length; a str clone falls to the
                // strlen path below.
                if count_expr_is_vec(inner, c, sigs) {
                    let (_, l, _) = vec_arg(inner, c, mdl, sigs);
                    l
                } else {
                    let p = emit_str_val(inner, c, mdl, sigs);
                    mdl.declare("declare i64 @strlen(ptr)".to_string());
                    let t = c.temp();
                    c.line(&format!("  {} = call i64 @strlen(ptr {})", t, p));
                    t
                }
            }
            other if count_expr_is_vec(other, c, sigs) => {
                let (_, l, _) = vec_arg(other, c, mdl, sigs);
                l
            }
            // Str-typed value expressions (loop results, bound heap
            // strings): the byte length via strlen.
            other => {
                let p = emit_str_val(other, c, mdl, sigs);
                mdl.declare("declare i64 @strlen(ptr)".to_string());
                let t = c.temp();
                c.line(&format!("  {} = call i64 @strlen(ptr {})", t, p));
                t
            }
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
            emit_let_scoped(binds, frees, c, mdl, sigs, |c, mdl| emit_int(body, c, mdl, sigs))
        }
        HExpr::Seq(vs) => {
            // `(do s… e)` yields `e`; only a statement-only tail yields 0.
            let Some((last, init)) = vs.split_last() else {
                return "0".to_string();
            };
            for v in init {
                emit_stmt(v, c, mdl, sigs);
            }
            let stmt_only = matches!(
                last,
                HExpr::Spit { .. } | HExpr::Exit(_) | HExpr::DropStmt(_)
            ) || matches!(last, HExpr::Call(n, _)
                if n == "print-f64" || n == "print-i64" || n == "print-str");
            if stmt_only {
                emit_stmt(last, c, mdl, sigs);
                "0".to_string()
            } else {
                emit_int(last, c, mdl, sigs)
            }
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
        HExpr::Loop { .. } => emit_loop_node(e, c, mdl, sigs),
        HExpr::Recur(..) => panic!("codegen: recur outside loop (validation bug)"),
        HExpr::Spit { path, data } => {
            let pv = emit_str_val(path, c, mdl, sigs);
            let dv = emit_str_val(data, c, mdl, sigs);
            mdl.declare("declare void @cato_spit(ptr, ptr)".to_string());
            c.line(&format!("  call void @cato_spit(ptr {}, ptr {})", pv, dv));
            "0".to_string()
        }
        HExpr::Exit(c2) => {
            let v = emit_int(c2, c, mdl, sigs);
            mdl.declare("declare void @cato_exit(i64)".to_string());
            c.line(&format!("  call void @cato_exit(i64 {})", v));
            "0".to_string()
        }
        HExpr::ToStr(_, _) | HExpr::ParseNum { .. } | HExpr::Slurp(_) | HExpr::Args => panic!(
            "codegen: string/Option/vector result must be bound with let"
        ),
        HExpr::StrNth { s, idx, .. } => {
            // Bounds-checked byte load, branchless: clamp the index with a
            // select before the GEP so out-of-range reads (and negatives)
            // yield 0, never a fault.
            let sp = emit_str_val(s, c, mdl, sigs);
            mdl.declare("declare i64 @strlen(ptr)".to_string());
            let len = c.temp();
            c.line(&format!("  {} = call i64 @strlen(ptr {})", len, sp));
            let iv = emit_int(idx, c, mdl, sigs);
            let inb = c.temp();
            c.line(&format!("  {} = icmp ult i64 {}, {}", inb, iv, len));
            let safe = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", safe, inb, iv));
            let p = c.temp();
            c.line(&format!(
                "  {} = getelementptr inbounds i8, ptr {}, i64 {}",
                p, sp, safe
            ));
            let b = c.temp();
            c.line(&format!("  {} = load i8, ptr {}, align 1", b, p));
            let z = c.temp();
            c.line(&format!("  {} = zext i8 {} to i64", z, b));
            let r = c.temp();
            c.line(&format!("  {} = select i1 {}, i64 {}, i64 0", r, inb, z));
            r
        }
        HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..)
        | HExpr::StrCmp(..) | HExpr::StrOrd(..) | HExpr::CmpW(..) | HExpr::SetContains { .. }
        | HExpr::ConstB(..) => {
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
        HExpr::Get { ty, .. } if ty.is_int() => emit_get(e, c, mdl, sigs),
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
        HExpr::ClosureCall { .. } => emit_closure_call(e, c, mdl, sigs),
        HExpr::Closure { .. } => {
            panic!("codegen: closure value must be bound with let")
        }
        HExpr::BinW(op, a, b, ty) => {
            let av = emit_int(a, c, mdl, sigs);
            let bv = emit_int(b, c, mdl, sigs);
            emit_int_binop(*op, &av, &bv, *ty, c, mdl)
        }
        HExpr::ICast(inner, from, target) => {
            let iv = emit_typed(inner, *from, c, mdl, sigs);
            emit_cast(iv, *from, *target, c)
        }
        HExpr::BinF(..) => panic!("codegen: f32 arithmetic in i64 context"),
        HExpr::SetLit(_)
        | HExpr::SetConj { .. }
        | HExpr::SetDisj { .. }
        | HExpr::Subs { .. }
        | HExpr::Split { .. }
        | HExpr::Join { .. }
        | HExpr::CharStr(..) => {
            panic!("codegen: heap value must be bound with let")
        }
        HExpr::Get { ty, .. } => panic!(
            "codegen: get returning {:?} in i64 context (expected F64 context)",
            ty
        ),
    }
    .to_string()
}


/// Can this expression only denote a vector (never a string)? Used to
/// route `count` results: vector values contribute their length, string
/// values their byte length. Calls resolve through the signature table,
/// control flow through its annotation, and lets through the body.
fn count_expr_is_vec(
    e: &HExpr,
    c: &FnCtx,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> bool {
    match e {
        HExpr::Call(name, _) => {
            matches!(sigs.get(name), Some((Ty::Vec(_), _)))
        }
        HExpr::If(_, _, _, t) | HExpr::Match { ty: t, .. } => matches!(t, Ty::Vec(_)),
        HExpr::Split { .. } | HExpr::Args => true,
        // These all produce vector headers: a loop annotated (Vector _), a
        // comprehension, an append, a slice copy, and a clone of anything
        // vector-valued. Missing arms here used to misroute `count` to the
        // strlen path, emitting ill-typed IR (a %VecI64 into `@strlen`).
        HExpr::Loop { ty, .. } => matches!(ty, Ty::Vec(_)),
        HExpr::For { .. } => true,
        HExpr::Conj { .. } => true,
        HExpr::Subvec { .. } => true,
        HExpr::Clone(inner) => count_expr_is_vec(inner, c, sigs),
        HExpr::Let(binds, body, _) => match body.as_ref() {
            // An inlined body aliases its bind: the bind's recorded type
            // decides (the binding itself is not emitted yet).
            HExpr::Var(v) => binds
                .iter()
                .any(|(n, _, t)| n == v && matches!(t, Ty::Vec(_))),
            b => count_expr_is_vec(b, c, sigs),
        },
        HExpr::Seq(vs) => vs.last().map(|x| count_expr_is_vec(x, c, sigs)).unwrap_or(false),
        HExpr::Var(v) => c.vecs.contains_key(v) || c.grow.contains_key(v),
        _ => false,
    }
}
