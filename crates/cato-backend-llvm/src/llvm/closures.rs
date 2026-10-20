use super::*;
use cato_core::Lambda;

/// Phase 14: first-class closures. A closure value is `%Closure { ptr, ptr }`:
/// the code pointer (an out-of-line `@lambda_N` whose first param is the env)
/// and a malloc'd env block holding the captures in slot order. Captures are
/// scalars or strings in this slice, so every env slot is 8 bytes.
///
/// Emit `(fn ...)` creation: malloc the env, store each capture, bind the
/// `{ code, env }` header. Returns a ptr to an alloca'd %Closure.
pub(crate) fn emit_closure_creation(
    mangled: &str,
    sig: u32,
    captures: &[(String, Ty)],
    capture_owns: &[bool],
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let _ = sig;
    let mangled = mangled.to_string();

    // Env block: one ownership-bitmap word, then the captures. Bit i records
    // whether the env owns capture i's heap (static literals and aliases are
    // excluded by the drop planner) so the glue frees only what it must.
    let n: i64 = 1 + captures.iter().map(|(_, t)| capture_slots(t)).sum::<i64>();
    let nreg = c.temp();
    c.line(&format!("  {} = add i64 {}, 0", nreg, n));
    let nbytes = c.temp();
    c.line(&format!("  {} = mul i64 {}, 8", nbytes, nreg));
    let env = emit_malloc(c, mdl, &nbytes);
    let mut bm: i64 = 0;
    for (i, owns_i) in capture_owns.iter().enumerate() {
        if *owns_i {
            bm |= 1 << i;
        }
    }
    let bword = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds i64, ptr {}, i64 0",
        bword, env
    ));
    c.line(&format!("  store i64 {}, ptr {}, align 8", bm, bword));
    let mut at: i64 = 1;
    for (name, ty) in captures.iter() {
        at += store_capture(env.clone(), at, name, *ty, c, mdl, sigs);
    }
    // The closure header.
    let slot = c.temp();
    c.prelude.push(format!("  {} = alloca %Closure, align 8", slot));
    let codef = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 0",
        codef, slot
    ));
    c.line(&format!("  store ptr @{}, ptr {}, align 8", mangled, codef));
    let envf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 1",
        envf, slot
    ));
    c.line(&format!("  store ptr {}, ptr {}, align 8", env, envf));
    let dropf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 2",
        dropf, slot
    ));
    c.line(&format!(
        "  store ptr @drop__{}, ptr {}, align 8",
        mangled, dropf
    ));
    slot
}

/// Env slots a capture occupies: 1 for scalars/bool/str/boxed ADT, 3 for
/// collection headers and for a captured closure's {code, env, drop}.
pub(crate) fn capture_slots(ty: &Ty) -> i64 {
    match ty {
        Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Fn(_) => 3,
        _ => 1,
    }
}

/// Store one capture at env slot offset `at`; returns the slots consumed.
fn store_capture(
    env: String,
    at: i64,
    name: &str,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> i64 {
    let gep = |c: &mut FnCtx, k: i64| {
        let g = c.temp();
        c.line(&format!(
            "  {} = getelementptr inbounds i64, ptr {}, i64 {}",
            g, env, at + k
        ));
        g
    };
    match ty {
        Ty::Vec(_) | Ty::Map | Ty::Set => {
            let (p, l, cap) = emit_collection_header(name, ty, c, mdl, sigs);
            let g0 = gep(c, 0);
            c.line(&format!("  store ptr {}, ptr {}, align 8", p, g0));
            let g1 = gep(c, 1);
            c.line(&format!("  store i64 {}, ptr {}, align 8", l, g1));
            let g2 = gep(c, 2);
            c.line(&format!("  store i64 {}, ptr {}, align 8", cap, g2));
            3
        }
        Ty::Fn(_) => {
            let slot = c
                .scalar
                .get(name)
                .map(|(r, _)| r.clone())
                .unwrap_or_else(|| panic!("codegen: {} is not a closure binding", name));
            for (k, field) in [0usize, 1usize, 2usize].iter().enumerate() {
                let f = c.temp();
                c.line(&format!(
                    "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 {}",
                    f, slot, field
                ));
                let w = c.temp();
                c.line(&format!("  {} = load ptr, ptr {}, align 8", w, f));
                let g = gep(c, k as i64);
                c.line(&format!("  store ptr {}, ptr {}, align 8", w, g));
            }
            3
        }
        _ => {
            let v = read_capture(name, ty, c, mdl, sigs);
            let g0 = gep(c, 0);
            match ty {
                Ty::F64 => c.line(&format!("  store double {}, ptr {}, align 8", v, g0)),
                Ty::F32 => c.line(&format!("  store float {}, ptr {}, align 8", v, g0)),
                // The env keeps its own copy (the str store rule, as for
                // constructor fields): the env frees its copy, the binding's
                // scope frees the original, and the closure may outlive the
                // binding.
                Ty::Str => {
                    mdl.ensure_decl("declare ptr @strdup(ptr)");
                    let d = c.temp();
                    c.line(&format!("  {} = call ptr @strdup(ptr {})", d, v));
                    c.line(&format!("  store ptr {}, ptr {}, align 8", d, g0));
                }
                t if t.is_int() => c.line(&format!(
                    "  store {} {}, ptr {}, align 8",
                    Mdl::ll_scalar(t),
                    v,
                    g0
                )),
                Ty::Bool => c.line(&format!("  store i1 {}, ptr {}, align 8", v, g0)),
                Ty::Adt(_) => {
                    c.line(&format!("  store ptr {}, ptr {}, align 8", v, g0));
                }
                o => panic!("codegen: capture of {:?} not supported", o),
            }
            1
        }
    }
}

/// Read a collection binding's live (ptr, len, cap) header.
fn emit_collection_header(
    name: &str,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> (String, String, String) {
    if let Some((slot, _)) = c.grow.get(name).cloned() {
        load_header(c, &slot)
    } else if let Some((slot, _)) = c.growm.get(name).cloned() {
        load_header(c, &slot)
    } else {
        let (p, l, _) = vec_arg(&HExpr::Var(name.to_string()), c, mdl, sigs);
        (p, l.clone(), l)
    }
}

fn load_header(c: &mut FnCtx, slot: &str) -> (String, String, String) {
    let mut out = Vec::new();
    for k in 0..3usize {
        let g = c.temp();
        c.line(&format!(
            "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 {}",
            g, slot, k
        ));
        if k == 0 {
            let l = c.temp();
            c.line(&format!("  {} = load ptr, ptr {}, align 8", l, g));
            out.push(l);
        } else {
            let l = c.temp();
            c.line(&format!("  {} = load i64, ptr {}, align 8", l, g));
            out.push(l);
        }
    }
    let cap = out.pop().unwrap();
    let l = out.pop().unwrap();
    let p = out.pop().unwrap();
    (p, l, cap)
}

/// Read the current value of a scalar/string/ADT capture for the env store.
fn read_capture(
    name: &str,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    match ty {
        Ty::F64 | Ty::F32 => emit_typed(&HExpr::Var(name.to_string()), ty, c, mdl, sigs),
        Ty::Str => emit_str_val(&HExpr::Var(name.to_string()), c, mdl, sigs),
        Ty::Bool => emit_bool(&HExpr::Var(name.to_string()), c, mdl, sigs),
        Ty::Adt(_) => emit_adt(&HExpr::Var(name.to_string()), c, mdl, sigs),
        t if t.is_int() => emit_typed(&HExpr::Var(name.to_string()), t, c, mdl, sigs),
        o => panic!("codegen: capture of {:?} not supported", o),
    }
}

/// Emit `(f args...)`: load the code/env pair, then an indirect call whose
/// first argument is the env block.
pub(crate) fn emit_closure_call(
    e: &HExpr,
    c: &mut FnCtx,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let HExpr::ClosureCall { f, args, sig } = e else {
        panic!("codegen: emit_closure_call on non-call")
    };
    // Mdl stores (ret, param tys).
    let (ret, ptys) = mdl
        .fn_sigs
        .get(*sig as usize)
        .cloned()
        .unwrap_or_else(|| panic!("codegen: unknown closure sig {}", sig));
    // The closure value: a ptr to a %Closure alloca (Var only in this slice).
    let fslot = match &**f {
        HExpr::Var(v) => c
            .scalar
            .get(v)
            .map(|(r, t)| {
                assert!(*t != Ty::I64, "codegen: {} is not a closure", v);
                r.clone()
            })
            .unwrap_or_else(|| panic!("codegen: unknown closure var {}", v)),
        other => panic!("codegen: closure callee must be a var, got {:?}", other),
    };
    let cf = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 0",
        cf, fslot
    ));
    let code = c.temp();
    c.line(&format!("  {} = load ptr, ptr {}, align 8", code, cf));
    let ef = c.temp();
    c.line(&format!(
        "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 1",
        ef, fslot
    ));
    let env = c.temp();
    c.line(&format!("  {} = load ptr, ptr {}, align 8", env, ef));

    let mut ll_args = vec![format!("ptr {}", env)];
    for (arg, ty) in args.iter().zip(ptys.iter()) {
        match ty {
            t if t.is_int() => {
                let v = emit_int(arg, c, mdl, sigs);
                ll_args.push(format!("{} {}", Mdl::ll_scalar(*t), v));
            }
            Ty::F64 => {
                let v = emit_float(arg, c, mdl, sigs);
                ll_args.push(format!("double {}", v));
            }
            Ty::F32 => {
                let v = emit_float(arg, c, mdl, sigs);
                ll_args.push(format!("float {}", v));
            }
            Ty::Bool => {
                let v = emit_bool(arg, c, mdl, sigs);
                ll_args.push(format!("i1 {}", v));
            }
            Ty::Str => {
                let v = emit_str_val(arg, c, mdl, sigs);
                ll_args.push(format!("ptr {}", v));
            }
            o => panic!("codegen: closure arg type {:?} not supported", o),
        }
    }
    let ret_ll = match ret {
        Ty::Bool => "i1".to_string(),
        Ty::F64 => "double".to_string(),
        Ty::F32 => "float".to_string(),
        Ty::Str => "ptr".to_string(),
        t if t.is_int() => Mdl::ll_scalar(t).to_string(),
        o => panic!("codegen: closure ret {:?} not supported", o),
    };
    let r = c.temp();
    let mut pl = vec!["ptr".to_string()];
    pl.extend(ptys.iter().map(|t| match t {
        t if t.is_int() => Mdl::ll_scalar(*t).to_string(),
        Ty::F64 => "double".to_string(),
        Ty::F32 => "float".to_string(),
        Ty::Bool => "i1".to_string(),
        Ty::Str => "ptr".to_string(),
        o => panic!("codegen: closure param {:?} not supported", o),
    }));
    c.line(&format!(
        "  {} = call {} ({}) {}({})",
        r,
        ret_ll,
        pl.join(", "),
        code,
        ll_args.join(", ")
    ));
    r
}

/// Emit an out-of-line lambda: `@lambda_N(ptr env, params...) -> ret`. The
/// captures are loaded from the env block into the scalar environment.
pub(crate) fn emit_lambda(
    lambda: &Lambda,
    mdl: &mut Mdl,
    sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let mut c = FnCtx::new();
    c.line("entry:");
    // Params.
    let mut params = vec!["ptr noalias nocapture readonly %env".to_string()];
    for (pname, pty) in &lambda.params {
        let r = format!("%{}", mangle(pname));
        match pty {
            t if t.is_int() => params.push(format!("{} {}", Mdl::ll_scalar(*t), r)),
            Ty::F64 => params.push(format!("double {}", r)),
            Ty::F32 => params.push(format!("float {}", r)),
            Ty::Bool => params.push(format!("i1 {}", r)),
            Ty::Str => params.push(format!("ptr {}", r)),
            o => panic!("codegen: lambda param type {:?} not supported", o),
        }
        c.scalar.insert(pname.clone(), (r, *pty));
    }
    // Load captures from the env block into the scalar map under their
    // original names (params never shadow captures: captures exclude them).
    let mut at: i64 = 1; /* env[0] is the ownership bitmap */
    for (cname, ty) in lambda.captures.iter() {
        match ty {
            Ty::Vec(_) | Ty::Map | Ty::Set => {
                // Rebuild a growable header slot from the env words.
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %VecI64, align 8", slot));
                for k in 0..3i64 {
                    let f = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds i64, ptr %env, i64 {}",
                        f,
                        at + k
                    ));
                    let v = c.temp();
                    c.line(&format!("  {} = load i64, ptr {}, align 8", v, f));
                    let dst = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds %VecI64, ptr {}, i32 0, i32 {}",
                        dst, slot, k
                    ));
                    if k == 0 {
                        let p = c.temp();
                        c.line(&format!("  {} = inttoptr i64 {} to ptr", p, v));
                        c.line(&format!("  store ptr {}, ptr {}, align 8", p, dst));
                    } else {
                        c.line(&format!("  store i64 {}, ptr {}, align 8", v, dst));
                    }
                }
                match ty {
                    Ty::Vec(elem2) => {
                        c.grow.insert(cname.clone(), (slot.clone(), *elem2));
                    }
                    _ => {
                        c.growm.insert(cname.clone(), (slot.clone(), *ty));
                    }
                }
                at += 3;
                continue;
            }
            Ty::Fn(_) => {
                let slot = c.temp();
                c.prelude
                    .push(format!("  {} = alloca %Closure, align 8", slot));
                for k in 0..3i64 {
                    let f = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds i64, ptr %env, i64 {}",
                        f,
                        at + k
                    ));
                    let v = c.temp();
                    c.line(&format!("  {} = load ptr, ptr {}, align 8", v, f));
                    let dst = c.temp();
                    c.line(&format!(
                        "  {} = getelementptr inbounds %Closure, ptr {}, i32 0, i32 {}",
                        dst, slot, k
                    ));
                    c.line(&format!("  store ptr {}, ptr {}, align 8", v, dst));
                }
                c.scalar.insert(cname.clone(), (slot, *ty));
                // A captured closure occupies three env words, as packed
                // (`capture_slots`).
                at += capture_slots(ty);
                continue;
            }
            _ => {}
        }
        let f = c.temp();
        c.line(&format!(
            "  {} = getelementptr inbounds i64, ptr %env, i64 {}",
            f, at
        ));
        at += capture_slots(ty);
        let v = c.temp();
        let scalar_ty = match ty {
            Ty::Bool => "i8",
            // Strings and boxed ADTs ride as one pointer word.
            Ty::Str | Ty::Adt(_) => "ptr",
            t => Mdl::ll_scalar(*t),
        };
        c.line(&format!("  {} = load {}, ptr {}, align 8", v, scalar_ty, f));
        let v2 = if *ty == Ty::Bool {
            let t = c.temp();
            c.line(&format!("  {} = trunc i8 {} to i1", t, v));
            t
        } else {
            v
        };
        c.scalar.insert(cname.clone(), (v2, *ty));
    }
    let ret_ll = match lambda.ret {
        Ty::Bool => "i1",
        Ty::F64 => "double",
        Ty::F32 => "float",
        Ty::Str => "ptr",
        t if t.is_int() => Mdl::ll_scalar(t),
        o => panic!("codegen: lambda ret {:?} not supported", o),
    };
    // Same caller-frees convention as plain fns: materialize borrowed-shape
    // str leaves at the lambda's return boundary.
    let body = if lambda.ret == Ty::Str {
        own_str_return(&lambda.body)
    } else {
        lambda.body.clone()
    };
    let val = emit_for_ret(&body, &mut c, mdl, sigs, lambda.ret);
    let mut text = format!(
        "define {} @{}({}) {{\n",
        ret_ll,
        lambda.mangled,
        params.join(", ")
    );
    text.push_str(&FnCtx::with_prelude(c.lines, &c.prelude).join("\n"));
    text.push_str("\n  ret ");
    // Bool returns widen back from i8 loads; ret_ll already i1 and val is i1.
    text.push_str(&format!("{} {}\n}}\n", ret_ll, val));
    text
}

fn c_temp_label(prefix: &str, n: usize) -> String {
    format!("%{}{}", prefix, n)
}

/// The drop-glue helper for a closure: `@drop__lambda_N(ptr env)`. Frees the
/// captures the env owns (per the ownership bitmap) in reverse slot order —
/// each through its type's own mechanism: string bytes, collection buffers,
/// boxed-ADT glue, a captured closure's own drop function — then frees the
/// env. The closure value is move-only: exactly one live header is
/// responsible for calling this.
pub(crate) fn emit_lambda_drop(
    lambda: &Lambda,
    mdl: &mut Mdl,
    _sigs: &HashMap<String, (Ty, Vec<Ty>)>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "define void @drop__{}(ptr %env) {{\nentry:\n",
        lambda.mangled
    ));
    // The ownership bitmap at env[0]: bit i set = the env owns capture i's
    // heap. Per capture (reverse slot order), branch to the free sequence
    // only when the bit is set.
    out.push_str("  %bm = load i64, ptr %env\n");
    let mut at: i64 = 1;
    let mut slots: Vec<(Ty, i64, usize)> = Vec::new();
    for (i, (_, ty)) in lambda.captures.iter().enumerate() {
        slots.push((*ty, at, i));
        at += capture_slots(ty);
    }
    let mut label: usize = 0;
    for (ty, off, bit) in slots.iter().rev() {
        let mask = format!("%m{}", label);
        out.push_str(&format!(
            "  {} = and i64 %bm, {}\n",
            mask,
            1i64 << bit
        ));
        let cond = format!("%c{}", label);
        out.push_str(&format!(
            "  {} = icmp ne i64 {}, 0\n",
            cond, mask
        ));
        let free_lbl = format!("own{}", label);
        let done_lbl = format!("done{}", label);
        label += 1;
        out.push_str(&format!(
            "  br i1 {}, label %{}, label %{}\n",
            cond, free_lbl, done_lbl
        ));
        out.push_str(&format!("{}:\n", free_lbl));
        let word = |k: i64, out: &mut String| {
            let f = format!(
                "  %w{}_{} = getelementptr inbounds i64, ptr %env, i64 {}\n",
                label, k, off + k
            );
            let l = format!("  %v{}_{} = load ptr, ptr %w{}_{}\n", label, k, label, k);
            out.push_str(&f);
            out.push_str(&l);
            format!("%v{}_{}", label, k)
        };
        match ty {
            Ty::Str | Ty::Vec(_) | Ty::Set => {
                let p = word(0, &mut out);
                mdl.ensure_decl("declare void @free(ptr)");
                out.push_str(&format!("  call void @free(ptr {})\n", p));
            }
            Ty::Map => {
                // Flat buffer or HAMT root: the runtime release decides.
                let p = word(0, &mut out);
                let capw = format!(
                    "  %cw{} = getelementptr inbounds i64, ptr %env, i64 {}\n",
                    label,
                    off + 2
                );
                let capl =
                    format!("  %cv{} = load i64, ptr %cw{}\n", label, label);
                out.push_str(&capw);
                out.push_str(&capl);
                mdl.ensure_decl("declare void @cato_map_release_pair(ptr, i64)");
                out.push_str(&format!(
                    "  call void @cato_map_release_pair(ptr {}, i64 %cv{})\n",
                    p, label
                ));
            }
            Ty::Adt(i) if mdl.adt_owns(*i) => {
                let p = word(0, &mut out);
                out.push_str(&format!(
                    "  call void @drop__{}(ptr {})\n",
                    mdl.adts[*i as usize].name, p
                ));
            }
            Ty::Fn(_) => {
                // {code, env, drop}: call the captured closure's own glue.
                let e = word(1, &mut out);
                let d = word(2, &mut out);
                out.push_str(&format!("  call void {}(ptr {})\n", d, e));
            }
            _ => {}
        }
        out.push_str(&format!("  br label %{}\n", done_lbl));
        out.push_str(&format!("{}:\n", done_lbl));
    }
    mdl.ensure_decl("declare void @free(ptr)");
    out.push_str("  call void @free(ptr %env)\n  ret void\n}\n");
    out
}
