use super::*;

pub(crate) fn emit_vec_call(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String, Elem) {
    let HExpr::Call(name, args) = e else {
        panic!("codegen: emit_vec_call on non-call")
    };
    let elem = match sigs.get(name).map(|(r, _)| *r) {
        Some(Ty::Vec(el)) => el,
        other => panic!("codegen: call {} does not return a vector ({:?})", name, other),
    };
    let st = call_user(name, args, c, mdl, sigs);
    let p = c.temp();
    c.line(&format!("  {} = extractvalue %VecI64 {}, 0", p, st));
    let l = c.temp();
    c.line(&format!("  {} = extractvalue %VecI64 {}, 1", l, st));
    (p, l, elem)
}

pub(crate) fn call_user_node(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match e {
        HExpr::Call(name, args) => call_user(name, args, c, mdl, sigs),
        other => panic!("codegen: expected a call, got {:?}", other),
    }
}

pub(crate) fn call_user(
    name: &str,
    args: &[HExpr],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {    let (ret_ty, arg_tys) = sigs.get(name).unwrap_or_else(|| panic!("codegen: unknown call {}", name));
    let arg_tys = arg_tys.clone();
    let mut ll_args = Vec::new();
    for (expr, ty) in args.iter().zip(arg_tys.iter()) {
        match ty {
            Ty::Vec(_) => {
                let (d, l, _) = emit_vec_pair(expr, c, mdl, sigs);
                ll_args.push(format!("ptr {}", d));
                ll_args.push(format!("i64 {}", l));
            }
            Ty::F64 => {
                let v = emit_float(expr, c, mdl, sigs);
                ll_args.push(format!("double {}", v));
            }
            t @ (Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize) => {
                let v = emit_int(expr, c, mdl, sigs);
                ll_args.push(format!("{} {}", Mdl::ll_scalar(*t), v));
            }
            Ty::F32 => {
                let v = emit_float(expr, c, mdl, sigs);
                ll_args.push(format!("float {}", v));
            }
            Ty::Bool => {
                let v = emit_bool(expr, c, mdl, sigs);
                ll_args.push(format!("i1 {}", v));
            }
            Ty::Param(_) => panic!("codegen: unresolved type parameter in call args"),
            Ty::Map => {
                let (p, l, cap) = map_val(expr, c, mdl, sigs);
                ll_args.push(format!("ptr {}", p));
                ll_args.push(format!("i64 {}", l));
                ll_args.push(format!("i64 {}", cap));
            }
            Ty::Set => {
                let (p, l) = set_val(expr, c, mdl, sigs);
                ll_args.push(format!("ptr {}", p));
                ll_args.push(format!("i64 {}", l));
            }
            Ty::Fn(_) => {
                let slot = match expr {
                    HExpr::Var(v) => c
                        .scalar
                        .get(v)
                        .map(|(r, _)| r.clone())
                        .unwrap_or_else(|| panic!("codegen: unknown closure var {}", v)),
                    o => panic!("codegen: closure arg must be a var, got {:?}", o),
                };
                let lv = c.temp();
                c.line(&format!(
                    "  {} = load %Closure, ptr {}, align 8",
                    lv, slot
                ));
                ll_args.push(format!("%Closure {}", lv));
            }
            Ty::Str => {
                let v = emit_str_val(expr, c, mdl, sigs);
                ll_args.push(format!("ptr {}", v));
            }
            Ty::Adt(i) => {
                let v = emit_adt(expr, c, mdl, sigs);
                if mdl.adt_boxed(*i) {
                    // Boxed: the owning pointer moves into the callee.
                    ll_args.push(format!("ptr {}", v));
                } else {
                    // By-value boundary: load the caller's slot into the arg.
                    let lv = c.temp();
                    c.line(&format!(
                        "  {} = load {}, ptr {}, align 8",
                        lv,
                        mdl.adt_name(*i),
                        v
                    ));
                    ll_args.push(format!("{} {}", mdl.adt_name(*i), lv));
                }
            }
        }
    }
    let adt_ret = matches!(ret_ty, Ty::Adt(_));
    let ret_ll = if *ret_ty == Ty::Bool {
        "i1".to_string()
    } else if ret_ty.is_int() || ret_ty.is_float() {
        Mdl::ll_scalar(*ret_ty).to_string()
    } else if let Ty::Adt(i) = *ret_ty {
        if mdl.adt_boxed(i) {
            "ptr".to_string()
        } else {
            mdl.adt_name(i)
        }
    } else if *ret_ty == Ty::Str {
        "ptr".to_string()
    } else if matches!(ret_ty, Ty::Vec(_) | Ty::Map | Ty::Set) {
        "%VecI64".to_string()
    } else {
        "i64".to_string()
    };
    let t = c.temp();
    c.line(&format!(
        "  {} = call {} @{}({})",
        t,
        ret_ll,
        mangle(name),
        ll_args.join(", ")
    ));
    if !adt_ret {
        return t;
    }
    // ADT return: boxed values come back as owning pointers directly; an
    // inline by-value struct is stored into a fresh caller slot so the
    // internal ptr-based representation holds.
    let i = match ret_ty {
        Ty::Adt(i) => *i,
        _ => unreachable!(),
    };
    if mdl.adt_boxed(i) {
        return t;
    }
    let slot = c.temp();
    c.prelude
        .push(format!("  {} = alloca {}, align 8", slot, mdl.adt_name(i)));
    c.line(&format!(
        "  store {} {}, ptr {}, align 8",
        mdl.adt_name(i),
        t,
        slot
    ));
    slot
}

// ---------------------------------------------------------------------------
// Free variable collection
// ---------------------------------------------------------------------------
