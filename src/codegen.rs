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
            loops.push(make_loop_spec(&mangle(&d.name), reduce, &d.params));
        } else {
            let body = rewrite_body(
                &d.body,
                &mangle(&d.name),
                &d.params,
                &mut idmap,
                &mut loops,
                &mut sigs,
            )
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

fn make_loop_spec(helper: &str, reduce: HReduce, params: &[(String, Ty)]) -> LoopSpec {
    let (acc_ty, mut free) = loop_free_vars(&reduce);
    let mut caps: Vec<(String, Ty)> = free
        .vars
        .drain(..)
        .map(|v| {
            let t = params
                .iter()
                .find(|(n, _)| *n == v)
                .map(|(_, t)| *t)
                .unwrap_or(Ty::F64);
            (v, t)
        })
        .collect();
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
            init,
            src,
            acc,
            el,
            body,
            maps,
            filters,
            ..
        } => {
            let mut bv = VarSet::new();
            bv.insert_expr(init);
            bv.insert_expr(src);
            for m in maps {
                bv.insert_expr(&m.body);
                bv.remove(&m.el);
            }
            for fl in filters {
                bv.insert_expr(&fl.body);
                bv.remove(&fl.el);
            }
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
    params: &[(String, Ty)],
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
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(rewrite_body(
            e, fnprefix, params, idmap, loops, sigs,
        )?)),
        HExpr::Count(e) => {
            HExpr::Count(Box::new(rewrite_body(e, fnprefix, params, idmap, loops, sigs)?))
        }
        HExpr::If(c, t, e, ty) => HExpr::If(
            Box::new(rewrite_body(c, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(t, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(e, fnprefix, params, idmap, loops, sigs)?),
            *ty,
        ),
        HExpr::And(a, b) => HExpr::And(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Or(a, b) => HExpr::Or(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Not(v) => HExpr::Not(Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?)),
        HExpr::Cmp(op, a, b, f) => HExpr::Cmp(
            op.clone(),
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *f,
        ),
        HExpr::Abs(v, t) => {
            HExpr::Abs(Box::new(rewrite_body(v, fnprefix, params, idmap, loops, sigs)?), *t)
        }
        HExpr::Min(a, b, t) => HExpr::Min(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::Max(a, b, t) => HExpr::Max(
            Box::new(rewrite_body(a, fnprefix, params, idmap, loops, sigs)?),
            Box::new(rewrite_body(b, fnprefix, params, idmap, loops, sigs)?),
            *t,
        ),
        HExpr::Call(name, args) => HExpr::Call(
            name.clone(),
            args.iter()
                .map(|a| rewrite_body(a, fnprefix, params, idmap, loops, sigs))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        HExpr::Let(binds, body) => HExpr::Let(
            binds
                .iter()
                .map(|(n, e, t)| {
                    Ok((
                        n.clone(),
                        rewrite_body(e, fnprefix, params, idmap, loops, sigs)?,
                        *t,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?,
            Box::new(rewrite_body(body, fnprefix, params, idmap, loops, sigs)?),
        ),
        HExpr::Seq(vs) => HExpr::Seq(
            vs.iter()
                .map(|v| rewrite_body(v, fnprefix, params, idmap, loops, sigs))
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
            let spec = make_loop_spec(&helper, (**r).clone(), params);
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
    blk: usize,
    cur: String,
    lines: Vec<String>,
    scalar: HashMap<String, (String, Ty)>,
    vecs: HashMap<String, (String, String)>,
}

impl FnCtx {
    fn new() -> Self {
        FnCtx {
            counter: 0,
            blk: 0,
            cur: String::new(),
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
    fn block_name(&mut self) -> String {
        let n = self.blk;
        self.blk += 1;
        format!("b{}", n)
    }
    fn open(&mut self, label: &str) -> String {
        let l = label.to_string();
        self.cur = l.clone();
        l
    }
    fn line(&mut self, s: &str) {
        self.lines.push(s.to_string());
    }
    fn clone_with_lines(&self, lines: Vec<String>) -> Self {
        FnCtx {
            counter: self.counter,
            blk: self.blk,
            cur: self.cur.clone(),
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
        let ty = if *t == Ty::F64 {
            "double"
        } else if *t == Ty::Bool {
            "i1"
        } else {
            "i64"
        };
        params.push(format!("{} %{}", ty, mangle(n)));
    }

    c.line("entry:");
    c.open("entry");
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

    let (filters, maps) = match &spec.reduce {
        HReduce::Closure { maps, filters, .. } | HReduce::Pipeline { maps, filters, .. } => {
            (filters, maps)
        }
    };

    match &spec.reduce {
        // Single-block closure: plain body, no stages, no control flow.
        HReduce::Closure { body, .. }
            if maps.is_empty() && filters.is_empty() && !has_ctl(body) =>
        {
            // Compute the loop-carried accumulator value first (its register
            // name becomes the phi's back-edge operand), then emit the block.
            let mut scratch = c.clone_with_lines(Vec::new());
            scratch.cur = "%loop".to_string();
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
            c.open("loop");
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
            c.line("  %p = getelementptr inbounds i64, ptr %data, i64 %i");
            c.line("  %x = load i64, ptr %p, align 8");

            if !filters.is_empty() {
                for fl in filters {
                    c.scalar.insert(fl.el.clone(), ("%x".to_string(), Ty::I64));
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
            let mut val = "%x".to_string();
            let mut val_ty = Ty::I64;
            for m in maps {
                c.scalar.insert(m.el.clone(), (val.clone(), m.el_ty));
                val = emit_typed(&m.body, m.res_ty, &mut c, mdl, sigs);
                val_ty = m.res_ty;
            }
            c.scalar.insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
            let r = match &spec.reduce {
                HReduce::Closure { body, .. } => {
                    c.scalar.insert(el.clone(), (val.clone(), val_ty));
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
            c.line(&format!("  {} = icmp ult i64 %i.next, %len", t_cond));
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
            Ty::Bool => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Bool));
                params.push(format!("i1 {}", r));
            }
        }
    }

    c.line("entry:");
    c.open("entry");
    if defn.name == "main" {
        for form in seq_forms(&defn.body) {
            emit_stmt(&form, &mut c, mdl, sigs);
        }
    } else {
        let val = emit_for_ret(&defn.body, &mut c, mdl, sigs, defn.ret);
        let ret_ll = if defn.ret == Ty::Bool { "i1" } else if defn.ret == Ty::F64 { "double" } else { "i64" };
        c.line(&format!("  ret {} {}", ret_ll, val));
    }

    let ret_ll = if defn.name == "main" {
        "i32".to_string()
    } else if defn.ret == Ty::Bool {
        "i1".to_string()
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
    match ret {
        Ty::F64 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        Ty::I64 => emit_int(e, c, mdl, sigs),
        Ty::VecI64 => panic!("codegen: cannot return a vector"),
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

/// Does this expression (or a stage / closure body) fork into basic blocks?
fn has_ctl(e: &HExpr) -> bool {
    match e {
        HExpr::If(..) | HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) => true,
        HExpr::Bin(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::ConvF64(x) | HExpr::Count(x) | HExpr::Abs(x, _) => has_ctl(x),
        HExpr::Call(_, args) => args.iter().any(has_ctl),
        HExpr::Let(binds, body) => binds.iter().any(|(_, e, _)| has_ctl(e)) || has_ctl(body),
        HExpr::Seq(vs) => vs.iter().any(has_ctl),
        HExpr::Cmp(..) => false,
        HExpr::Var(_) | HExpr::ConstI64(_) | HExpr::ConstF64(_) | HExpr::VecLit(_) => false,
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => has_ctl(a) || has_ctl(b),
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline { init, maps, filters, src, .. } => {
                has_ctl(init)
                    || has_ctl(src)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
            HReduce::Closure { init, src, body, maps, filters, .. } => {
                has_ctl(init)
                    || has_ctl(src)
                    || has_ctl(body)
                    || maps.iter().any(|m| has_ctl(&m.body))
                    || filters.iter().any(|f| has_ctl(&f.body))
            }
        },
    }
}

/// Pin a bare literal into a virtual register before a `br`/`phi`.
fn pin_value(ll_ty: &str, r: &str, c: &mut FnCtx) -> String {
    if r.starts_with('%') {
        return r.to_string();
    }
    let t = c.temp();
    if ll_ty == "double" {
        c.line(&format!("  {} = fadd double {}, 0.000000e+00", t, r));
    } else {
        c.line(&format!("  {} = add i64 {}, 0", t, r));
    }
    t
}

fn emit_typed(
    e: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match ty {
        Ty::F64 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        Ty::I64 => emit_int(e, c, mdl, sigs),
        Ty::VecI64 => panic!("codegen: control flow cannot produce a vector"),
    }
}

fn emit_bool(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    match e {
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

fn emit_if_node(
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
    let lty = if ty == Ty::F64 { "double" } else if ty == Ty::Bool { "i1" } else { "i64" };
    c.line(&format!(
        "  {} = phi {} [ {}, %{} ], [ {}, %{} ]",
        res, lty, tv, tp, ev, ep
    ));
    res
}

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
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
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
        HExpr::If(cond, then, els, t) => emit_if_node(cond, then, els, *t, c, mdl, sigs),
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
        HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Cmp(..) => {
            panic!("codegen: bool expression in i64 context")
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
            Ty::Bool => {
                let v = emit_bool(value, c, mdl, sigs);
                c.scalar.insert(name.clone(), (v, Ty::Bool));
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
                ll_args.push(format!("i64 {}", v));
            }
            Ty::Bool => {
                let v = emit_bool(expr, c, mdl, sigs);
                ll_args.push(format!("i1 {}", v));
            }
        }
    }
    let ret_ll = if *ret_ty == Ty::Bool {
        "i1"
    } else if *ret_ty == Ty::F64 {
        "double"
    } else {
        "i64"
    };
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
        HExpr::If(c, t, e, _) => {
            collect_vars(c, f);
            collect_vars(t, f);
            collect_vars(e, f);
        }
        HExpr::And(a, b) | HExpr::Or(a, b) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Not(v) => collect_vars(v, f),
        HExpr::Cmp(_, a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
        HExpr::Abs(v, _) => collect_vars(v, f),
        HExpr::Min(a, b, _) | HExpr::Max(a, b, _) => {
            collect_vars(a, f);
            collect_vars(b, f);
        }
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
                init,
                src,
                body,
                maps,
                filters,
                ..
            } => {
                collect_vars(init, f);
                collect_vars(src, f);
                for m in maps {
                    collect_vars(&m.body, f);
                }
                for fl in filters {
                    collect_vars(&fl.body, f);
                }
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