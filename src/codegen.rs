use crate::hir::*;
use std::collections::HashMap;

/// LLVM IR emitter.
///
/// Every `reduce` (top-level or nested) is hoisted into a dedicated loop
/// function whose vector-data argument is declared `noalias nocapture
/// readonly`, with `inbounds` GEPs and `align 8` loads, so LLVM can
/// vectorize the fused loop (spec §4.7).
pub fn emit_module(fns: &[Defn]) -> String {
    // Signature table: callee name -> (ret, param types). Includes declared
    // defns and hoisted loop helpers.
    let mut sigs: HashMap<String, (Ty, Vec<Ty>)> = HashMap::new();
    for d in fns {
        sigs.insert(
            d.name.clone(),
            (d.ret, d.params.iter().map(|(_, t)| *t).collect()),
        );
    }

    let mut loops: Vec<LoopSpec> = Vec::new();
    let mut idmap: HashMap<String, usize> = HashMap::new();
    let mut plain: Vec<Defn> = Vec::new();
    let mut mdl = Mdl::new();

    for d in fns {
        if matches!(d.body, HExpr::Reduce(_)) {
            // Top-level loop defn: the function IS the helper.
            let reduce = match &d.body {
                HExpr::Reduce(r) => (**r).clone(),
                _ => unreachable!(),
            };
            loops.push(make_loop_spec(&mangle(&d.name), reduce));
        } else {
            let body = rewrite_body(&d.body, &mangle(&d.name), &mut idmap, &mut loops, &mut sigs)
                .unwrap_or_else(|e| panic!("codegen: {}", e));
            let mut nd = d.clone();
            nd.body = body;
            plain.push(nd);
        }
    }

    let mut output: Vec<String> = Vec::new();

    output.push("; cato: generated LLVM IR (vertical slice)".to_string());
    output.push("%VecI64 = type { ptr, i64, i64 }".to_string());
    output.push(String::new());
    output.push("declare void @cato_print_f64(double)".to_string());
    output.push("declare void @cato_print_i64(i64)".to_string());
    output.push(String::new());

    for spec in &loops {
        output.push(String::new());
        output.push(emit_loop(spec, &mut mdl, &sigs));
    }
    for defn in &plain {
        output.push(String::new());
        output.push(emit_plain(defn, &mut mdl, &sigs));
    }

    // Vec-literal globals minted during emission belong at module top.
    for g in mdl.globals() {
        let i = output
            .iter()
            .position(|l| l.starts_with("declare void @cato_print_i64"))
            .unwrap_or(0);
        output.insert(i + 1, g.clone());
        output.insert(i + 1, String::new());
    }

    output.join("\n")
}

// ---------------------------------------------------------------------------
// Reduce hoisting
// ---------------------------------------------------------------------------

struct LoopSpec {
    helper: String,
    acc_ty: Ty, // I64 or F64, from the init literal
    caps: Vec<(String, Ty)>,
    reduce: HReduce,
}

fn make_loop_spec(helper: &str, reduce: HReduce) -> LoopSpec {
    let (acc_ty, mut free) = loop_free_vars(&reduce);
    let mut caps: Vec<(String, Ty)> = free.vars.drain(..).map(|v| (v, Ty::F64)).collect();
    caps.sort_by(|a, b| a.0.cmp(&b.0));
    LoopSpec {
        helper: helper.to_string(),
        acc_ty,
        caps,
        reduce,
    }
}

fn src_var_of(reduce: &HReduce) -> String {
    let src = match reduce {
        HReduce::Pipeline { src, .. } | HReduce::Closure { src, .. } => src,
    };
    match &**src {
        HExpr::Var(v) => v.clone(),
        other => panic!("codegen: loop src must be a var, got {:?}", other),
    }
}

fn loop_free_vars(reduce: &HReduce) -> (Ty, VarSet) {
    let src_var = src_var_of(reduce);
    match reduce {
        HReduce::Pipeline {
            init, maps, filters, ..
        } => {
            let mut bv = VarSet::new();
            bv.insert_expr(init);
            for m in maps {
                bv.insert_expr(&m.body);
                bv.remove(&m.el);
            }
            for fl in filters {
                bv.insert_expr(&fl.body);
                bv.remove(&fl.el);
            }
            bv.remove(&src_var);
            let t = if matches!(init, HExpr::ConstF64(_)) { Ty::F64 } else { Ty::I64 };
            (t, bv)
        }
        HReduce::Closure {
            init, src, acc, el, body, ..
        } => {
            let mut bv = VarSet::new();
            bv.insert_expr(init);
            bv.insert_expr(src);
            bv.insert_expr(body);
            bv.remove(&src_var);
            bv.remove(acc);
            bv.remove(el);
            let t = if matches!(init, HExpr::ConstF64(_)) { Ty::F64 } else { Ty::I64 };
            (t, bv)
        }
    }
}

fn acc_ret_is_f64(r: &HReduce) -> bool {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => {
            matches!(init, HExpr::ConstF64(_))
        }
    }
}

/// Replace every `reduce` node with a call to its hoisted helper, collecting
/// the helpers. Returns the rewritten expression.
fn rewrite_body(
    h: &HExpr,
    fnprefix: &str,
    idmap: &mut HashMap<String, usize>,
    loops: &mut Vec<LoopSpec>,
    sigs: &mut HashMap<String, (Ty, Vec<Ty>)>,
) -> Result<HExpr, String> {
    Ok(match h {
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
        HExpr::Bin(op, a, b) => HExpr::Bin(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, idmap, loops, sigs)?),
        ),
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(rewrite_body(
            e, fnprefix, idmap, loops, sigs,
        )?)),
        HExpr::Count(e) => HExpr::Count(Box::new(rewrite_body(e, fnprefix, idmap, loops, sigs)?)),
        HExpr::Call(name, args) => HExpr::Call(
            name.clone(),
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Let(binds, body) => HExpr::Let(
            binds
                .iter()
                .map(|(n, e, t)| {
                    Ok((
                        n.clone(),
                        rewrite_body(e, fnprefix, idmap, loops, sigs)?,
                        *t,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?,
            Box::new(rewrite_body(body, fnprefix, idmap, loops, sigs)?),
        ),
        HExpr::Seq(vs) => HExpr::Seq(
            vs.iter()
                .map(|v| rewrite_body(v, fnprefix, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Reduce(r) => {
            let src_var = match &**r {
                HReduce::Pipeline { src, .. } | HReduce::Closure { src, .. } => match &**src {
                    HExpr::Var(s) => s.clone(),
                    _ => return Err("reduce source must be a vector variable".into()),
                },
            };
            let id = idmap.entry(fnprefix.to_string()).or_insert(0);
            let helper = format!("{}_reduce_{}", fnprefix, id);
            *id += 1;

            let ret_f64 = acc_ret_is_f64(r);
            let ret_ty = if ret_f64 { Ty::F64 } else { Ty::I64 };
            let spec = make_loop_spec(&helper, (**r).clone());
            // Signature: src vec + captures, ret = loop accumulator type.
            let mut arg_tys = vec![Ty::VecI64];
            for (_, t) in &spec.caps {
                arg_tys.push(*t);
            }
            sigs.insert(helper.clone(), (ret_ty, arg_tys));
            loops.push(spec);

            let mut args = vec![HExpr::Var(src_var)];
            let caps = &loops.last().unwrap().caps.clone();
            for (cname, _) in caps {
                args.push(HExpr::Var(cname.clone()));
            }
            HExpr::Call(helper, args)
        }
    })
}

// ---------------------------------------------------------------------------
// Module buffer
// ---------------------------------------------------------------------------

struct Mdl {
    vec_globals: Vec<String>,
    counter: usize,
}

impl Mdl {
    fn new() -> Self {
        Mdl {
            vec_globals: Vec::new(),
            counter: 0,
        }
    }
    fn mint_vec(&mut self, lit: &[i64]) -> (String, String) {
        let id = self.counter;
        self.counter += 1;
        let label = format!("@g_vec_{}", id);
        let items: Vec<String> = lit.iter().map(|v| format!("i64 {}", v)).collect();
        self.vec_globals.push(format!(
            "{} = private constant [{} x i64] [{}]",
            label,
            lit.len(),
            items.join(", ")
        ));
        (label, lit.len().to_string())
    }
    fn globals(&self) -> &[String] {
        &self.vec_globals
    }
}

// ---------------------------------------------------------------------------
// Function emitter
// ---------------------------------------------------------------------------

struct FnCtx {
    counter: usize,
    lines: Vec<String>,
    scalar: HashMap<String, (String, Ty)>,
    vecs: HashMap<String, (String, String)>,
}

impl FnCtx {
    fn new() -> Self {
        FnCtx {
            counter: 0,
            lines: Vec::new(),
            scalar: HashMap::new(),
            vecs: HashMap::new(),
        }
    }
    fn temp(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("%t{}", n)
    }
    fn line(&mut self, s: &str) {
        self.lines.push(s.to_string());
    }
    fn clone_with_lines(&self, lines: Vec<String>) -> Self {
        FnCtx {
            counter: self.counter,
            lines,
            scalar: self.scalar.clone(),
            vecs: self.vecs.clone(),
        }
    }
}

fn mangle(name: &str) -> String {
    name.replace('-', "_")
}

fn llvm_f64(v: f64) -> String {
    // LLVM decimal FP constants need an explicit decimal point before 'e'
    // (Rust's {:e} renders "0e0", which LLVM reads as an integer constant).
    let s = format!("{:e}", v);
    if let Some(pos) = s.find('e') {
        if s[..pos].contains('.') {
            s
        } else {
            format!("{}.0{}", &s[..pos], &s[pos..])
        }
    } else if s.contains('.') {
        s
    } else {
        format!("{}.0", s)
    }
}

// ---------------------------------------------------------------------------
// Loop function emission
// ---------------------------------------------------------------------------

fn emit_loop(spec: &LoopSpec, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    for (n, t) in &spec.caps {
        c.scalar.insert(n.clone(), (format!("%{}", mangle(n)), *t));
    }
    let acc_ll = if spec.acc_ty == Ty::F64 { "double" } else { "i64" };
    let init_val = loop_init(&spec.reduce);
    let init_lit = if spec.acc_ty == Ty::F64 {
        llvm_f64(init_val)
    } else {
        init_val.to_string()
    };

    let mut params = vec![
        "ptr noalias nocapture readonly %data".to_string(),
        "i64 %len".to_string(),
    ];
    for (n, t) in &spec.caps {
        let ty = if *t == Ty::F64 { "double" } else { "i64" };
        params.push(format!("{} %{}", ty, mangle(n)));
    }

    c.line("entry:");
    let t_isempty = c.temp();
    c.line(&format!("  {} = icmp eq i64 %len, 0", t_isempty));
    c.line(&format!("  br i1 {}, label %ret, label %loop", t_isempty));

    // Element / accumulator bindings shared by all stages.
    let el = match &spec.reduce {
        HReduce::Closure { el, .. } => el.clone(),
        HReduce::Pipeline { maps, filters, .. } => {
            if let Some(m) = maps.first() {
                m.el.clone()
            } else if let Some(f) = filters.first() {
                f.el.clone()
            } else {
                "x".to_string()
            }
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

    match &spec.reduce {
        HReduce::Closure { body, .. } => {
            // Compute the loop-carried accumulator value first (its register
            // name becomes the phi's back-edge operand), then emit the block.
            let mut scratch = c.clone_with_lines(Vec::new());
            scratch.scalar.insert(el.clone(), ("%x".to_string(), Ty::I64));
            scratch.scalar.insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let val = if spec.acc_ty == Ty::F64 {
                emit_float(body, &mut scratch, mdl, sigs)
            } else {
                emit_int(body, &mut scratch, mdl, sigs)
            };
            c.counter = scratch.counter;
            c.line("");
            c.line("loop:");
            c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]");
            c.line(&format!(
                "  %acc = phi {} [ {}, %entry ], [ {}, %loop ]",
                acc_ll, init_lit, val
            ));
            c.line("  %p = getelementptr inbounds i64, ptr %data, i64 %i");
            c.line("  %x = load i64, ptr %p, align 8");
            c.scalar.insert(el.clone(), ("%x".to_string(), Ty::I64));
            c.scalar.insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            c.lines.extend(scratch.lines);
            carried = val;
            carried_block = "%loop";
            c.line("  %i.next = add i64 %i, 1");
            let t_cond = c.temp();
            c.line(&format!("  {} = icmp ult i64 %i.next, %len", t_cond));
            c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
        }
        HReduce::Pipeline {
            combine, init, maps, filters, ..
        } => {
            let _ = init;
            let has_maps = !maps.is_empty();
            let has_filters = !filters.is_empty();

            if !has_maps && !has_filters {
                c.line("");
                c.line("loop:");
                c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]");
                c.line(&format!(
                    "  %acc = phi {} [ {}, %entry ], [ %acc.next, %loop ]",
                    acc_ll, init_lit
                ));
                c.line("  %p = getelementptr inbounds i64, ptr %data, i64 %i");
                c.line("  %x = load i64, ptr %p, align 8");
                c.scalar.insert(el.clone(), ("%x".to_string(), Ty::I64));
                c.scalar.insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
                let combined = combine_step(*combine, "%x", "%acc", spec.acc_ty, &mut c);
                c.line(&format!("  %acc.next = {}", combined));
                carried = "%acc.next".to_string();
                carried_block = "%loop";
                c.line("  %i.next = add i64 %i, 1");
                let t_cond = c.temp();
                c.line(&format!("  {} = icmp ult i64 %i.next, %len", t_cond));
                c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
            } else {
                c.line("");
                c.line("loop:");
                c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %merge ]");
                c.line(&format!(
                    "  %acc = phi {} [ {}, %entry ], [ %acc.merge, %merge ]",
                    acc_ll, init_lit
                ));
                c.line("  %p = getelementptr inbounds i64, ptr %data, i64 %i");
                c.line("  %x = load i64, ptr %p, align 8");

                if has_filters {
                    for fl in filters {
                        c.scalar.insert(fl.el.clone(), ("%x".to_string(), Ty::I64));
                        let pv = emit_int(&fl.body, &mut c, mdl, sigs);
                        let t_keep = c.temp();
                        c.line(&format!("  {} = icmp ne i64 {}, 0", t_keep, pv));
                        c.line(&format!("  br i1 {}, label %apply, label %skip", t_keep));
                    }
                } else {
                    c.line("  br label %apply");
                }

                c.line("");
                c.line("apply:");
                let mut val = "%x".to_string();
                for m in maps {
                    c.scalar.insert(m.el.clone(), (val.clone(), Ty::I64));
                    val = emit_int(&m.body, &mut c, mdl, sigs);
                }
                c.scalar.insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
                let combined = combine_step(*combine, &val, "%acc", spec.acc_ty, &mut c);
                c.line(&format!("  %acc2 = {}", combined));
                c.line("  br label %merge");

                c.line("");
                c.line("skip:");
                c.line("  br label %merge");

                c.line("");
                c.line("merge:");
                c.line(&format!(
                    "  %acc.merge = phi {} [ %acc2, %apply ], [ %acc, %skip ]",
                    acc_ll
                ));
                c.line("  %i.next = add i64 %i, 1");
                carried = "%acc.merge".to_string();
                carried_block = "%merge";
                let t_cond = c.temp();
                c.line(&format!("  {} = icmp ult i64 %i.next, %len", t_cond));
                c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
            }
        }
    }

    c.line("");
    c.line("ret:");
    let t_ret = c.temp();
    c.line(&format!(
        "  {} = phi {} [ {}, %entry ], [ {}, {} ]",
        t_ret, acc_ll, init_lit, carried, carried_block
    ));
    c.line(&format!("  ret {} {}", acc_ll, t_ret));

    let mut text = format!("define {} @{}(", acc_ll, spec.helper);
    text.push_str(&params.join(", "));
    text.push_str(") {\n");
    text.push_str(&c.lines.join("\n"));
    text.push('\n');
    text.push_str("}\n");
    text
}

fn loop_init(r: &HReduce) -> f64 {
    match r {
        HReduce::Pipeline { init, .. } | HReduce::Closure { init, .. } => match init {
            HExpr::ConstF64(v) => *v,
            HExpr::ConstI64(v) => *v as f64,
            _ => 0.0,
        },
    }
}

fn combine_step(op: BinOp, val: &str, acc: &str, ty: Ty, _c: &mut FnCtx) -> String {
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

fn emit_plain(defn: &Defn, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    let mut params = Vec::new();
    for (pname, pty) in &defn.params {
        match pty {
            Ty::VecI64 => {
                let d = format!("%{}_data", mangle(pname));
                let l = format!("%{}_len", mangle(pname));
                c.vecs.insert(pname.clone(), (d.clone(), l.clone()));
                params.push(format!("ptr noalias nocapture readonly {}", d));
                params.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::F64));
                params.push(format!("double {}", r));
            }
            Ty::I64 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::I64));
                params.push(format!("i64 {}", r));
            }
        }
    }

    c.line("entry:");
    if defn.name == "main" {
        for form in seq_forms(&defn.body) {
            emit_stmt(&form, &mut c, mdl, sigs);
        }
    } else {
        let val = emit_for_ret(&defn.body, &mut c, mdl, sigs, defn.ret);
        let ret_ll = if defn.ret == Ty::F64 { "double" } else { "i64" };
        c.line(&format!("  ret {} {}", ret_ll, val));
    }

    let ret_ll = if defn.name == "main" {
        "i32".to_string()
    } else if defn.ret == Ty::F64 {
        "double".to_string()
    } else {
        "i64".to_string()
    };
    let mut text = format!("define {} @{}(", ret_ll, mangle(&defn.name));
    text.push_str(&params.join(", "));
    text.push_str(") {\n");
    text.push_str(&c.lines.join("\n"));
    if defn.name == "main" {
        text.push('\n');
        text.push_str("  ret i32 0\n");
    }
    text.push_str("}\n");
    text
}

fn seq_forms(b: &HExpr) -> Vec<HExpr> {
    match b {
        HExpr::Seq(vs) => vs.clone(),
        other => vec![other.clone()],
    }
}

fn emit_for_ret(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>, ret: Ty) -> String {
    if ret == Ty::F64 {
        emit_float(e, c, mdl, sigs)
    } else {
        emit_int(e, c, mdl, sigs)
    }
}

fn emit_stmt(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
    match e {
        HExpr::Call(name, args) if name == "print-f64" => {
            let v = emit_float(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_f64(double {})", v));
        }
        HExpr::Call(name, args) if name == "print-i64" => {
            let v = emit_int(&args[0], c, mdl, sigs);
            c.line(&format!("  call void @cato_print_i64(i64 {})", v));
        }
        _ => {
            emit_for_ret(e, c, mdl, sigs, Ty::I64);
        }
    }
}

// ---------------------------------------------------------------------------
// Expression emission
// ---------------------------------------------------------------------------

fn emit_int(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstI64(v) => v.to_string(),
        HExpr::Reduce(_) => panic!("codegen: Reduce in i64 context (rewrite bug)"),
        HExpr::Var(v) => {
            if let Some((r, _)) = c.scalar.get(v) {
                r.clone()
            } else {
                // vector var used in length context
                let (_, l) = c.vecs.get(v).expect("known var");
                l.clone()
            }
        }
        HExpr::Count(v) => {
            let HExpr::Var(s) = &**v else {
                panic!("codegen: count of non-var")
            };
            let (_, l) = c.vecs.get(s).expect("count of vector var");
            l.clone()
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
        HExpr::Call(name, args) => {
            if name == "even?" {
                let v = emit_int(&args[0], c, mdl, sigs);
                let p = c.temp();
                c.line(&format!("  {} = and i64 {}, 1", p, v));
                // `even?` is true when the low bit is clear, so invert `and`.
                let t = c.temp();
                c.line(&format!("  {} = xor i64 {}, 1", t, p));
                t
            } else {
                call_user(name, args, c, mdl, sigs)
            }
        }
        HExpr::Let(binds, body) => {
            emit_let(binds, c, mdl, sigs);
            emit_int(body, c, mdl, sigs)
        }
        HExpr::Seq(vs) => {
            for v in vs {
                emit_stmt(v, c, mdl, sigs);
            }
            "0".to_string()
        }
        HExpr::ConstF64(v) => panic!("codegen: ConstF64 {} in i64 context", v),
        HExpr::VecLit(_) => panic!("codegen: vec literal in i64 context"),
    }
    .to_string()
}

fn emit_float(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
        HExpr::ConstF64(v) => llvm_f64(*v),
        HExpr::Var(v) => {
            let (r, _) = c.scalar.get(v).expect("known f64 var");
            r.clone()
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
        HExpr::ConvF64(v) => {
            let iv = emit_int(v, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = sitofp i64 {} to double", t, iv));
            t
        }
        HExpr::Call(name, args) => {
            if name == "print-f64" {
                let v = emit_float(&args[0], c, mdl, sigs);
                c.line(&format!("  call void @cato_print_f64(double {})", v));
                "0.0".to_string()
            } else {
                call_user(name, args, c, mdl, sigs)
            }
        }
        HExpr::Let(binds, body) => {
            emit_let(binds, c, mdl, sigs);
            emit_float(body, c, mdl, sigs)
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
        other => panic!("codegen: unsupported node in f64 context: {:?}", other),
    }
}

fn emit_let(binds: &[(String, HExpr, Ty)], c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
    for (name, value, ty) in binds {
        match ty {
            Ty::F64 => {
                let v = emit_float(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::F64));
            }
            Ty::VecI64 => materialize_vec_lit(value, name, c, mdl),
            _ => {
                let v = emit_int(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::I64));
            }
        }
    }
}

fn materialize_vec_lit(value: &HExpr, name: &str, c: &mut FnCtx, mdl: &mut Mdl) {
    let HExpr::VecLit(lit) = value else {
        panic!("codegen: can only materialize vector literal bindings")
    };
    let (label, len) = mdl.mint_vec(lit);
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
    c.vecs.insert(name.to_string(), (t_d, t_l));
}

fn vec_arg(expr: &HExpr, c: &mut FnCtx, mdl: &mut Mdl) -> (String, String) {
    match expr {
        HExpr::Var(v) => c
            .vecs
            .get(v)
            .cloned()
            .unwrap_or_else(|| panic!("codegen: {} is not a vector binding", v)),
        HExpr::VecLit(lit) => {
            let (label, len) = mdl.mint_vec(lit);
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
            (t_d, t_l)
        }
        _ => panic!("codegen: unsupported vector argument"),
    }
}

fn call_user(
    name: &str,
    args: &[HExpr],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let (ret_ty, arg_tys) = sigs.get(name).unwrap_or_else(|| panic!("codegen: unknown call {}", name));
    let arg_tys = arg_tys.clone();
    let mut ll_args = Vec::new();
    for (expr, ty) in args.iter().zip(arg_tys.iter()) {
        match ty {
            Ty::VecI64 => {
                let (d, l) = vec_arg(expr, c, mdl);
                ll_args.push(format!("ptr {}", d));
                ll_args.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let v = emit_float(expr, c, mdl, sigs);
                ll_args.push(format!("double {}", v));
            }
            Ty::I64 => {
                let v = emit_int(expr, c, mdl, sigs);
                ll_args.push(v);
            }
        }
    }
    let ret_ll = if *ret_ty == Ty::F64 { "double" } else { "i64" };
    let t = c.temp();
    c.line(&format!(
        "  {} = call {} @{}({})",
        t,
        ret_ll,
        mangle(name),
        ll_args.join(", ")
    ));
    t
}

// ---------------------------------------------------------------------------
// Free variable collection
// ---------------------------------------------------------------------------

#[derive(Default)]
struct VarSet {
    vars: Vec<String>,
}

impl VarSet {
    fn new() -> Self {
        Self { vars: Vec::new() }
    }
    fn remove(&mut self, v: &str) {
        self.vars.retain(|x| x != v);
    }
    fn insert_expr(&mut self, e: &HExpr) {
        collect_vars(e, &mut |v: String| {
            if !self.vars.contains(&v) {
                self.vars.push(v);
            }
        });
    }
}

fn collect_vars<F: FnMut(String)>(h: &HExpr, f: &mut F) {
    match h {
        HExpr::Var(v) => f(v.clone()),
        HExpr::Bin(_, a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::ConvF64(e) => collect_vars(e, f),
        HExpr::Count(e) => collect_vars(e, f),
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline {
                init, maps, filters, src, ..
            } => {
                collect_vars(init, f);
                collect_vars(src, f);
                for m in maps {
                    collect_vars(&m.body, f);
                }
                for fl in filters {
                    collect_vars(&fl.body, f);
                }
            }
            HReduce::Closure {
                init, src, body, ..
            } => {
                collect_vars(init, f);
                collect_vars(src, f);
                collect_vars(body, f);
            }
        },
        HExpr::Call(_, args) => {
            for a in args {
                collect_vars(a, f);
            }
        }
        HExpr::Let(binds, body) => {
            for (_, e, _) in binds {
                collect_vars(e, f);
            }
            collect_vars(body, f);
        }
        HExpr::Seq(vs) => {
            for v in vs {
                collect_vars(v, f);
            }
        }
        HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::VecLit(_) => {}
    }
}