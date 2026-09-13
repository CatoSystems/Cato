use super::*;

pub(crate) fn emit_plain(defn: &Defn, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> String {
    let mut c = FnCtx::new();
    let mut params = Vec::new();
    for (pname, pty) in &defn.params {
        match pty {
            Ty::Vec(e) => {
                let d = format!("%{}_data", mangle(pname));
                let l = format!("%{}_len", mangle(pname));
                c.vecs.insert(pname.clone(), (d.clone(), l.clone(), *e));
                params.push(format!("ptr noalias nocapture readonly {}", d));
                params.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::F64));
                params.push(format!("double {}", r));
            }
            t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), *t));
                params.push(format!("{} {}", Mdl::ll_scalar(*t), r));
            }
            Ty::F32 => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::F32));
                params.push(format!("float {}", r));
            }
            Ty::Bool => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Bool));
                params.push(format!("i1 {}", r));
            }
            Ty::Str => {
                let r = format!("%{}", mangle(pname));
                c.scalar.insert(pname.clone(), (r.clone(), Ty::Str));
                params.push(format!("ptr {}", r));
            }
            Ty::Map => panic!("codegen: map params not supported (maps are local values)"),
            Ty::Set => panic!("codegen: set params not supported (sets are local values)"),
            Ty::Fn(_) => {
                // Closure params arrive by value; copy into an entry slot.
                let r = format!("%{}", mangle(pname));
                params.push(format!("%Closure {}", r));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %Closure, align 8", slot));
                c.prelude
                    .push(format!("  store %Closure {}, ptr {}, align 8", r, slot));
                c.scalar.insert(pname.clone(), (slot, *pty));
            }
            Ty::Param(_) => panic!("codegen: unresolved type parameter in params"),
            Ty::Adt(i) => {
                // By-value boundary: the incoming struct is copied into an
                // entry slot; all internal uses go through the ptr.
                let r = format!("%{}", mangle(pname));
                params.push(format!("{} {}", mdl.adt_name(*i), r));
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(*i)));
                c.prelude.push(format!(
                    "  store {} {}, ptr {}, align 8",
                    mdl.adt_name(*i),
                    r,
                    slot
                ));
                c.scalar.insert(pname.clone(), (slot, *pty));
            }
        }
    }

    c.line("entry:");
    c.open("entry");
    if defn.name == "main" {
        for form in seq_forms(&defn.body) {
            emit_stmt(&form, &mut c, mdl, sigs);
        }
    } else if let Ty::Vec(e) = defn.ret {
        // Vector return: the %VecI64 struct goes back by value.
        let (p, l, _) = emit_vec_pair(&defn.body, &mut c, mdl, sigs);
        let _ = e;
        let s0 = c.temp();
        c.line(&format!(
            "  {} = insertvalue %VecI64 undef, ptr {}, 0",
            s0, p
        ));
        let s1 = c.temp();
        c.line(&format!(
            "  {} = insertvalue %VecI64 {}, i64 {}, 1",
            s1, s0, l
        ));
        let _ = e;
        c.line(&format!("  ret %VecI64 {}", s1));
    } else {
        let val = emit_for_ret(&defn.body, &mut c, mdl, sigs, defn.ret);
        if let Ty::Adt(i) = defn.ret {
            // Copy the result slot out by value; a returned ptr would dangle.
            let lv = c.temp();
            c.line(&format!(
                "  {} = load {}, ptr {}, align 8",
                lv,
                mdl.adt_name(i),
                val
            ));
            c.line(&format!("  ret {} {}", mdl.adt_name(i), lv));
        } else {
            let ret_ll = if defn.ret == Ty::Bool {
                "i1"
            } else if defn.ret.is_int() || defn.ret.is_float() {
                Mdl::ll_scalar(defn.ret)
            } else {
                "i64"
            };
            c.line(&format!("  ret {} {}", ret_ll, val));
        }
    }

    let ret_ll = if defn.name == "main" {
        "i32".to_string()
    } else if defn.ret == Ty::Bool {
        "i1".to_string()
    } else if defn.ret == Ty::F64 {
        "double".to_string()
    } else if let Ty::Adt(i) = defn.ret {
        mdl.adt_name(i)
    } else if matches!(defn.ret, Ty::Vec(_)) {
        "%VecI64".to_string()
    } else if defn.ret.is_int() || defn.ret.is_float() {
        Mdl::ll_scalar(defn.ret).to_string()
    } else {
        "i64".to_string()
    };
    let mut text = format!("define {} @{}(", ret_ll, mangle(&defn.name));
    text.push_str(&params.join(", "));
    text.push_str(") {\n");
    text.push_str(&FnCtx::with_prelude(c.lines, &c.prelude).join("\n"));
    if defn.name == "main" {
        text.push('\n');
        text.push_str("  ret i32 0\n");
    }
    text.push_str("}\n");
    text
}

/// FFI declaration: a plain LLVM `declare` — direct C calls, no marshaling.
pub(crate) fn emit_extern_decl(defn: &Defn, _mdl: &mut Mdl) -> String {
    let mut params = Vec::new();
    for (_, pty) in &defn.params {
        match pty {
            Ty::Vec(e) => {
                params.push(format!("ptr noalias nocapture readonly %data",));
                params.push("i64 %len".to_string());
                let _ = e;
            }
            Ty::F64 => params.push("double".to_string()),
            t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => params.push(Mdl::ll_scalar(*t).to_string()),
            Ty::F32 => params.push("float".to_string()),
            Ty::Bool => params.push("i1".to_string()),
            Ty::Str => params.push("ptr".to_string()),
            Ty::Map => panic!("codegen: map params not supported"),
            Ty::Set => panic!("codegen: set params not supported"),
            Ty::Fn(_) => panic!("codegen: closure params not supported on externs"),
            Ty::Adt(_) | Ty::Param(_) => {
                panic!("codegen: extern params must be scalars, strings or vectors")
            }
        }
    }
    let ret_ll = match defn.ret {
        Ty::I64 => "i64",
        Ty::F64 => "double",
        Ty::Bool => "i1",
        Ty::Str => "ptr",
        _ => panic!("codegen: extern return must be i64, f64, bool or str"),
    };
    let line = format!(
        "declare {} @{}({})",
        ret_ll,
        mangle(&defn.name),
        params.join(", ")
    );
    line
}

pub(crate) fn seq_forms(b: &HExpr) -> Vec<HExpr> {
    match b {
        HExpr::Seq(vs) => vs.clone(),
        other => vec![other.clone()],
    }
}

pub(crate) fn emit_for_ret(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>, ret: Ty) -> String {
    match ret {
        Ty::F64 | Ty::F32 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
            let _ = t;
            emit_int(e, c, mdl, sigs)
        }
        Ty::Adt(_) => emit_adt(e, c, mdl, sigs),
        Ty::Str => emit_str_val(e, c, mdl, sigs),
        Ty::Vec(_) => panic!("codegen: cannot return a vector"),
        Ty::Map => panic!("codegen: cannot return a map (maps are local values)"),
        Ty::Set => panic!("codegen: cannot return a set (sets are local values)"),
        Ty::Fn(_) => panic!("codegen: cannot return a closure yet"),
        Ty::Param(_) => panic!("codegen: unresolved type parameter"),
    }
}

pub(crate) fn emit_stmt(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String, (Ty, Vec<Ty>)>) {
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

pub(crate) fn emit_typed(
    e: &HExpr,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match ty {
        Ty::F64 | Ty::F32 => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
            let _ = t;
            emit_int(e, c, mdl, sigs)
        }
        Ty::Adt(_) => emit_adt(e, c, mdl, sigs),
        Ty::Str => emit_str_val(e, c, mdl, sigs),
        Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Fn(_) => {
            panic!("codegen: control flow cannot produce an aggregate")
        }
        Ty::Param(_) => panic!("codegen: unresolved type parameter"),
    }
}

pub(crate) fn emit_vec_pair(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String, Elem) {
    match e {
        HExpr::Var(v) => {
            if c.grow.contains_key(v) {
                grow_val(c, v)
            } else {
                vec_arg(e, c, mdl)
            }
        }
        HExpr::VecLit(_) => vec_arg(e, c, mdl),
        HExpr::Call(..) => emit_vec_call(e, c, mdl, sigs),
        HExpr::Let(binds, body, frees) => {
            emit_let(binds, c, mdl, sigs);
            let v = emit_vec_pair(body, c, mdl, sigs);
            emit_let_frees(frees, c, mdl);
            v
        }
        HExpr::Seq(vs) => {
            for v in &vs[..vs.len() - 1] {
                emit_stmt(v, c, mdl, sigs);
            }
            emit_vec_pair(vs.last().unwrap(), c, mdl, sigs)
        }
        other => panic!("codegen: unsupported vector value {:?}", other),
    }
}

/// Static width of an i64/i32 expression (for cast selection).
pub(crate) fn static_int_width(e: &HExpr, c: &FnCtx, sigs: &HashMap<String, (Ty, Vec<Ty>)>) -> Ty {
    match e {
        HExpr::ConstI64(_) => Ty::I64,
        HExpr::ConstF64(_) => Ty::F64,
        HExpr::ConvF64(_) => Ty::F64,
        HExpr::ICast(_, t) => *t,
        HExpr::BinW(.., t) => *t,
        HExpr::BinF(..) => Ty::F32,
        HExpr::Abs(_, t) | HExpr::Min(_, _, t) | HExpr::Max(_, _, t) => *t,
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(_, t)| *t)
            .unwrap_or(Ty::I64),
        HExpr::Call(name, _) => sigs
            .get(name)
            .map(|(r, _)| *r)
            .unwrap_or(Ty::I64),
        _ => Ty::I64,
    }
}

