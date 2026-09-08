use crate::ast::Expr;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    I64,
    F64,
    VecI64,
    Bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HExpr {
    ConstI64(i64),
    ConstF64(f64),
    Var(String),
    Bin(BinOp, Box<HExpr>, Box<HExpr>),
    ConvF64(Box<HExpr>),
    Count(Box<HExpr>),
    VecLit(Vec<i64>),
    Reduce(Box<HReduce>),
    Call(String, Vec<HExpr>),
    Let(Vec<(String, HExpr, Ty)>, Box<HExpr>),
    Seq(Vec<HExpr>),
    If(Box<HExpr>, Box<HExpr>, Box<HExpr>, Ty),
    And(Box<HExpr>, Box<HExpr>),
    Or(Box<HExpr>, Box<HExpr>),
    Not(Box<HExpr>),
    Cmp(CmpOp, Box<HExpr>, Box<HExpr>, bool),
    Abs(Box<HExpr>, Ty),
    Min(Box<HExpr>, Box<HExpr>, Ty),
    Max(Box<HExpr>, Box<HExpr>, Ty),
}

#[derive(Debug, Clone, PartialEq)]
pub enum HReduce {
    Pipeline {
        combine: BinOp,
        init: HExpr,
        maps: Vec<MapSpec>,
        filters: Vec<FilterSpec>,
        src: Box<HExpr>,
    },
    Closure {
        init: HExpr,
        src: Box<HExpr>,
        acc: String,
        el: String,
        body: HExpr,
        maps: Vec<MapSpec>,
        filters: Vec<FilterSpec>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapSpec {
    pub el: String,
    pub body: HExpr,
    pub el_ty: Ty,
    pub res_ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterSpec {
    pub el: String,
    pub body: HExpr,
    pub el_ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Defn {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    pub body: HExpr,
}

type Env = HashMap<String, Ty>;

struct RawDefn {
    name: String,
    params: Vec<(String, Ty)>,
    ret: Ty,
    body: Expr,
}

struct Ctx<'a> {
    raw_by_name: &'a HashMap<&'a str, &'a RawDefn>,
}

pub fn build(prog: &[Expr]) -> Result<Vec<Defn>, String> {
    // Pass 1: parse every defn into a raw (un-lowered) form without lowering,
    // so map/filter and calls can resolve any function regardless of order.
    let raws: Vec<RawDefn> = prog.iter().map(parse_raw).collect::<Result<_, _>>()?;
    let raw_by_name: HashMap<&str, &RawDefn> = raws
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();
    let ctx = Ctx { raw_by_name: &raw_by_name };

    let mut out = Vec::new();
    for raw in &raws {
        let mut env = Env::new();
        for (pname, pty) in &raw.params {
            env.insert(pname.clone(), *pty);
        }
        let body = match &raw.body {
            Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
                lower_body_forms(items, &env, &ctx)?
            }
            _ => lower(&raw.body, &env, &ctx)?,
        };
        out.push(Defn {
            name: raw.name.clone(),
            params: raw.params.clone(),
            ret: raw.ret,
            body,
        });
    }
    Ok(out)
}

fn parse_raw(form: &Expr) -> Result<RawDefn, String> {
    let Expr::List(items) = form else {
        return Err("expected (defn ...)".into());
    };
    let Some(Expr::LitSym(head)) = items.first() else {
        return Err("expected defn".into());
    };
    if head != "defn" {
        return Err(format!("unknown top-level form {}", head));
    }
    let Expr::LitSym(name) = &items[1] else {
        return Err("defn name must be a symbol".into());
    };
    let Expr::Vector(params) = &items[2] else {
        return Err("defn params must be a vector".into());
    };
    let params = parse_params(params)?;

    let mut body_start = 3;
    let mut ret = Ty::I64;
    if items.len() > body_start + 1 {
        if let Expr::LitSym(sep) = &items[body_start] {
            if sep == ":-" {
                if let Expr::LitSym(ty) = &items[body_start + 1] {
                    ret = parse_ty(ty)?;
                }
                body_start += 2;
            }
        }
    }

    let body = if items.len() > body_start {
        if items.len() - body_start == 1 {
            items[body_start].clone()
        } else {
            // Multiple body forms -> implicit sequence.
            Expr::List(std::iter::once(Expr::LitSym("do".to_string()))
                .chain(items[body_start..].iter().cloned())
                .collect())
        }
    } else {
        return Err("defn body required".into());
    };

    Ok(RawDefn {
        name: name.clone(),
        params,
        ret,
        body,
    })
}

fn parse_params(params: &[Expr]) -> Result<Vec<(String, Ty)>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < params.len() {
        let Expr::LitSym(name) = &params[i] else {
            return Err("param must be a symbol".into());
        };
        if i + 2 < params.len() {
            if let Expr::LitSym(sep) = &params[i + 1] {
                if sep == ":-" {
                    if let Expr::LitSym(ty) = &params[i + 2] {
                        out.push((name.clone(), parse_ty(ty)?));
                        i += 3;
                        continue;
                    }
                }
            }
        }
        return Err(format!("function parameter '{}' must have a type annotation (param :- ty)", name));
    }
    Ok(out)
}

fn parse_ty(s: &str) -> Result<Ty, String> {
    match s {
        "i64" => Ok(Ty::I64),
        "f64" => Ok(Ty::F64),
        "vec" => Ok(Ty::VecI64),
        "bool" => Ok(Ty::Bool),
        other => Err(format!("unknown type annotation '{}'", other)),
    }
}

fn lower_body_forms(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    // items is either [LitSym("do"), form...] for multiple bodies, or we
    // receive a single list whose first element is the actual body head.
    let Some(Expr::LitSym(head)) = items.first() else {
        return Err("expected body".into());
    };
    if head == "do" {
        let mut seq = Vec::new();
        for f in &items[1..] {
            seq.push(lower(f, env, ctx)?);
        }
        Ok(if seq.len() == 1 {
            seq.pop().unwrap()
        } else {
            HExpr::Seq(seq)
        })
    } else {
        Err("internal: expected implicit do".into())
    }
}

fn lower(expr: &Expr, env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    match expr {
        Expr::LitI64(v) => Ok(HExpr::ConstI64(*v)),
        Expr::LitF64(v) => Ok(HExpr::ConstF64(*v)),
        Expr::LitStr(s) => Ok(HExpr::Call("string-literal".into(), vec![HExpr::Var(s.clone())])),
        Expr::LitSym(s) => Ok(HExpr::Var(s.to_string())),
        Expr::Vector(items) => {
            if items.iter().all(|e| matches!(e, Expr::LitI64(_))) {
                let vs = items
                    .iter()
                    .map(|e| if let Expr::LitI64(v) = e { *v } else { unreachable!() })
                    .collect();
                Ok(HExpr::VecLit(vs))
            } else {
                Err("vector literal not supported (use (vec-i64 ...))".into())
            }
        }
        Expr::List(items) => {
            let Some(Expr::LitSym(head)) = items.first() else {
                return Err("form head must be a symbol".into());
            };
            match head.as_str() {
                "do" => {
                    let mut seq = Vec::new();
                    for f in &items[1..] {
                        seq.push(lower(f, env, ctx)?);
                    }
                    Ok(if seq.len() == 1 {
                        seq.pop().unwrap()
                    } else {
                        HExpr::Seq(seq)
                    })
                }
                "fn" => Err("lambda must be consumed by reduce/map/filter".into()),
                "let" => lower_let(items, env, ctx),
                "reduce" => lower_reduce(items, env, ctx),
                "map" | "filter" => Err(format!(
                    "{} must appear as the source of a reduce in the slice",
                    head
                )),
                "f64" => {
                    if items.len() != 2 {
                        return Err("f64 expects exactly 1 argument".into());
                    }
                    let inner = lower(&items[1], env, ctx)?;
                    Ok(HExpr::ConvF64(Box::new(inner)))
                }
                "if" => lower_if(items, env, ctx),
                "and" | "or" => lower_and_or(head, items, env, ctx),
                "not" => lower_not(items, env, ctx),
                "=" | "/=" | "<" | ">" | "<=" | ">=" => lower_cmp(head, items, env, ctx),
                "abs" => lower_abs(items, env, ctx),
                "min" | "max" => lower_min_max(head, items, env, ctx),
                "count" => {
                    if items.len() != 2 {
                        return Err("count expects exactly 1 argument".into());
                    }
                    let inner = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Count(Box::new(inner)))
                }
                "vec-i64" => {
                    let mut vs = Vec::new();
                    for it in &items[1..] {
                        match it {
                            Expr::LitI64(v) => vs.push(*v),
                            _ => return Err("vec-i64 elements must be i64 literals".into()),
                        }
                    }
                    Ok(HExpr::VecLit(vs))
                }
                "print-f64" => {
                    if items.len() != 2 {
                        return Err("print-f64 expects exactly 1 argument".into());
                    }
                    let a = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Call("print-f64".into(), vec![a]))
                }
                "print-i64" => {
                    if items.len() != 2 {
                        return Err("print-i64 expects exactly 1 argument".into());
                    }
                    let a = lower(&items[1], env, ctx)?;
                    Ok(HExpr::Call("print-i64".into(), vec![a]))
                }
                "+" | "-" | "*" | "/" => {
                    if items.len() != 3 {
                        return Err(format!("binary op {} expects 2 operands", head));
                    }
                    let op = match head.as_str() {
                        "+" => BinOp::Add,
                        "-" => BinOp::Sub,
                        "*" => BinOp::Mul,
                        _ => BinOp::Div,
                    };
                    let a = lower(&items[1], env, ctx)?;
                    let b = lower(&items[2], env, ctx)?;
                    Ok(promote_bin(op, a, b, env, ctx))
                }
                _ => {
                    let mut args = Vec::new();
                    for it in &items[1..] {
                        args.push(lower(it, env, ctx)?);
                    }
                    Ok(HExpr::Call(head.clone(), args))
                }
            }
        }
    }
}

fn coerce_f64(e: HExpr, cur: Ty, want: bool) -> HExpr {
    if want && cur != Ty::F64 {
        HExpr::ConvF64(Box::new(e))
    } else {
        e
    }
}

fn promote_pair(a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> (HExpr, HExpr) {
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    let f = at == Ty::F64 || bt == Ty::F64;
    (coerce_f64(a, at, f), coerce_f64(b, bt, f))
}

fn promote_bin(op: BinOp, a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> HExpr {
    let (a, b) = promote_pair(a, b, env, ctx);
    HExpr::Bin(op, Box::new(a), Box::new(b))
}

fn lower_if(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 4 {
        return Err("if expects (if cond then else)".into());
    }
    let cond = lower(&items[1], env, ctx)?;
    if ty_of(&cond, env, ctx) != Ty::Bool {
        return Err("if condition must be boolean".into());
    }
    let then = lower(&items[2], env, ctx)?;
    let els = lower(&items[3], env, ctx)?;
    let tt = ty_of(&then, env, ctx);
    let et = ty_of(&els, env, ctx);
    if tt != et && tt != Ty::F64 && et != Ty::F64 {
        return Err(format!(
            "if branches must be the same type, got {:?} and {:?}",
            tt, et
        ));
    }
    let t = if tt == Ty::F64 || et == Ty::F64 {
        Ty::F64
    } else {
        tt
    };
    Ok(HExpr::If(
        Box::new(cond),
        Box::new(coerce_f64(then, tt, t == Ty::F64)),
        Box::new(coerce_f64(els, et, t == Ty::F64)),
        t,
    ))
}

fn lower_cmp(op: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 {
        return Err(format!("comparison {} expects 2 operands", op));
    }
    let op = match op {
        "=" => CmpOp::Eq,
        "/=" => CmpOp::Ne,
        "<" => CmpOp::Lt,
        ">" => CmpOp::Gt,
        "<=" => CmpOp::Le,
        ">=" => CmpOp::Ge,
        _ => unreachable!(),
    };
    let (a, b) = promote_pair(
        lower(&items[1], env, ctx)?,
        lower(&items[2], env, ctx)?,
        env,
        ctx,
    );
    let is_f64 = ty_of(&a, env, ctx) == Ty::F64;
    Ok(HExpr::Cmp(op, Box::new(a), Box::new(b), is_f64))
}

fn lower_and_or(kind: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 {
        return Err(format!("{} expects 2 operands", kind));
    }
    let a = lower(&items[1], env, ctx)?;
    let b = lower(&items[2], env, ctx)?;
    if ty_of(&a, env, ctx) != Ty::Bool || ty_of(&b, env, ctx) != Ty::Bool {
        return Err(format!("{} expects boolean operands", kind));
    }
    Ok(if kind == "and" {
        HExpr::And(Box::new(a), Box::new(b))
    } else {
        HExpr::Or(Box::new(a), Box::new(b))
    })
}

fn lower_not(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 2 {
        return Err("not expects 1 operand".into());
    }
    let e = lower(&items[1], env, ctx)?;
    if ty_of(&e, env, ctx) != Ty::Bool {
        return Err("not expects a boolean operand".into());
    }
    Ok(HExpr::Not(Box::new(e)))
}

fn lower_abs(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 2 {
        return Err("abs expects 1 operand".into());
    }
    let e = lower(&items[1], env, ctx)?;
    let t = ty_of(&e, env, ctx);
    if t != Ty::I64 && t != Ty::F64 {
        return Err("abs expects an i64 or f64 operand".into());
    }
    Ok(HExpr::Abs(Box::new(e), t))
}

fn lower_min_max(kind: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 {
        return Err(format!("{} expects 2 operands", kind));
    }
    let (a, b) = promote_pair(
        lower(&items[1], env, ctx)?,
        lower(&items[2], env, ctx)?,
        env,
        ctx,
    );
    let t = ty_of(&a, env, ctx);
    if t != Ty::I64 && t != Ty::F64 {
        return Err(format!("{} expects i64/f64 operands", kind));
    }
    Ok(if kind == "min" {
        HExpr::Min(Box::new(a), Box::new(b), t)
    } else {
        HExpr::Max(Box::new(a), Box::new(b), t)
    })
}

fn ty_of(e: &HExpr, env: &Env, ctx: &Ctx) -> Ty {
    match e {
        HExpr::ConstI64(_) => Ty::I64,
        HExpr::ConstF64(_) => Ty::F64,
        HExpr::Var(s) => env.get(s).copied().unwrap_or(Ty::I64),
        HExpr::Bin(_, a, b) => {
            if ty_of(a, env, ctx) == Ty::F64 || ty_of(b, env, ctx) == Ty::F64 {
                Ty::F64
            } else {
                Ty::I64
            }
        }
        HExpr::ConvF64(_) => Ty::F64,
        HExpr::Count(_) => Ty::I64,
        HExpr::VecLit(_) => Ty::VecI64,
        HExpr::Reduce(r) => match &**r {
            HReduce::Closure { init, .. } => {
                if matches!(init, HExpr::ConstF64(_)) { Ty::F64 } else { Ty::I64 }
            }
            HReduce::Pipeline { init, .. } => {
                if matches!(init, HExpr::ConstF64(_)) { Ty::F64 } else { Ty::I64 }
            }
        },
        HExpr::Call(name, _) if name == "even?" => Ty::Bool,
        HExpr::Call(name, _) => ctx
            .raw_by_name
            .get(name.as_str())
            .map(|r| r.ret)
            .unwrap_or(Ty::I64),
        HExpr::Let(_, b) => ty_of(b, env, ctx),
        HExpr::Seq(v) => v.last().map(|x| ty_of(x, env, ctx)).unwrap_or(Ty::I64),
        HExpr::If(_, _, _, t) => *t,
        HExpr::And(..) | HExpr::Or(..) | HExpr::Not(_) | HExpr::Cmp(..) => Ty::Bool,
        HExpr::Abs(_, t) | HExpr::Min(_, _, t) | HExpr::Max(_, _, t) => *t,
    }
}

fn lower_let(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    let Expr::Vector(binds) = &items[1] else {
        return Err("let bindings must be a vector".into());
    };
    if binds.len() % 2 != 0 {
        return Err("let bindings must be key/value pairs".into());
    }
    let mut child = env.clone();
    let mut resolved = Vec::new();
    let mut i = 0;
    while i < binds.len() {
        let Expr::LitSym(name) = &binds[i] else {
            return Err("let binding name must be a symbol".into());
        };
        let value = lower(&binds[i + 1], &mut child, ctx)?;
        let t = ty_of(&value, &child, ctx);
        child.insert(name.clone(), t);
        resolved.push((name.clone(), value, t));
        i += 2;
    }
    let body = lower(&items[2], &child, ctx)?;
    Ok(HExpr::Let(resolved, Box::new(body)))
}

fn lower_reduce(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() < 4 {
        return Err(format!(
            "reduce expects (reduce <op|fn> <init> <src>), got {} arguments",
            items.len().saturating_sub(1)
        ));
    }
    let head = &items[1];
    match head {
        Expr::LitSym(op) if matches!(op.as_str(), "+" | "-" | "*" | "/") => {
            let combine = match op.as_str() {
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                _ => BinOp::Div,
            };
            let init = lower(&items[2], env, ctx)?;
            let (src, maps, filters) = pipeline_src(&items[3], env, ctx)?;
            Ok(HExpr::Reduce(Box::new(HReduce::Pipeline {
                combine,
                init,
                maps,
                filters,
                src: Box::new(src),
            })))
        }
        _ => {
            let Expr::List(fnitems) = head else {
                return Err("reduce expects a combine op or (fn ...)".into());
            };
            if fnitems.len() < 3 {
                return Err("reduce fn expects (fn [acc el] <body>)".into());
            }
            let Expr::Vector(params) = &fnitems[1] else {
                return Err("reduce fn params".into());
            };
            if params.len() < 2 {
                return Err("reduce fn must bind exactly [acc el]".into());
            }
            let acc = param_name(&params[0])?;
            let el = param_name(&params[1])?;
            let init = lower(&items[2], env, ctx)?;
            let init_ty = ty_of(&init, env, ctx);
            let (src, maps, filters) = pipeline_src(&items[3], env, ctx)?;
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
            let mut child = env.clone();
            child.insert(acc.clone(), init_ty);
            child.insert(el.clone(), el_ty);
            let body = lower(&fnitems[2], &child, ctx)?;
            Ok(HExpr::Reduce(Box::new(HReduce::Closure {
                init,
                src: Box::new(src),
                acc,
                el,
                body,
                maps,
                filters,
            })))
        }
    }
}

fn param_name(e: &Expr) -> Result<String, String> {
    match e {
        Expr::LitSym(s) => Ok(s.clone()),
        _ => Err("lambda param must be a symbol".into()),
    }
}

// Peel (map f inner) / (filter p inner) wrappers into specs, inner-most first,
// returning (base_source, maps, filters). f/p may be an inline (fn [el] body)
// or a reference to a defined function (e.g. square) or a builtin (even?).
fn pipeline_src(
    src: &Expr,
    env: &Env,
    ctx: &Ctx,
) -> Result<(HExpr, Vec<MapSpec>, Vec<FilterSpec>), String> {
    let Expr::List(items) = src else {
        return Ok((lower(src, env, ctx)?, Vec::new(), Vec::new()));
    };
    let Some(Expr::LitSym(head)) = items.first() else {
        return Ok((lower(src, env, ctx)?, Vec::new(), Vec::new()));
    };
    if head == "map" {
        if items.len() < 3 {
            return Err("map expects (map <fn> <src>)".into());
        }
        let (inner, mut maps, filters) = pipeline_src(&items[2], env, ctx)?;
        let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
        let (el, body, res_ty) = lambda_spec(&items[1], el_ty, env, ctx)?;
        maps.push(MapSpec {
            el,
            body,
            el_ty,
            res_ty,
        });
        // maps is inner-first; push keeps it inner-first.
        Ok((inner, maps, filters))
    } else if head == "filter" {
        if items.len() < 3 {
            return Err("filter expects (filter <fn> <src>)".into());
        }
        let (inner, maps, mut filters) = pipeline_src(&items[2], env, ctx)?;
        let (el, body, _) = lambda_spec(&items[1], Ty::I64, env, ctx)?;
        filters.push(FilterSpec {
            el,
            body,
            el_ty: Ty::I64,
        });
        Ok((inner, maps, filters))
    } else {
        Ok((lower(src, env, ctx)?, Vec::new(), Vec::new()))
    }
}

fn lambda_spec(
    f: &Expr,
    el_ty: Ty,
    env: &Env,
    ctx: &Ctx,
) -> Result<(String, HExpr, Ty), String> {
    match f {
        Expr::List(items) => {
            // Inline (fn [el] body)
            let Some(Expr::LitSym(h)) = items.first() else {
                return Err("map/filter arg must be a function".into());
            };
            if h != "fn" {
                return Err("map/filter arg must be (fn ...)".into());
            }
            if items.len() < 3 {
                return Err("fn expects (fn [el] <body>)".into());
            }
            let Expr::Vector(params) = &items[1] else {
                return Err("fn params".into());
            };
            if params.is_empty() {
                return Err("fn must bind an element param".into());
            }
            let el = param_name(&params[0])?;
            let mut child = env.clone();
            child.insert(el.clone(), el_ty);
            let body = lower(&items[2], &child, ctx)?;
            let res_ty = ty_of(&body, &child, ctx);
            Ok((el, body, res_ty))
        }
        Expr::LitSym(name) => resolve_function_ref(name, el_ty, env, ctx),
        _ => Err("map/filter arg must be a function".into()),
    }
}

fn resolve_function_ref(
    name: &str,
    el_ty: Ty,
    env: &Env,
    ctx: &Ctx,
) -> Result<(String, HExpr, Ty), String> {
    if name == "even?" {
        return Ok((
            "x".to_string(),
            HExpr::Call("even?".to_string(), vec![HExpr::Var("x".to_string())]),
            Ty::Bool,
        ));
    }
    match ctx.raw_by_name.get(name) {
        Some(raw) => {
            if raw.params.len() != 1 {
                return Err(format!("function {} used in map/filter must have 1 param", name));
            }
            let el = raw.params[0].0.clone();
            let mut child = env.clone();
            child.insert(el.clone(), el_ty);
            let body = match &raw.body {
                Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
                    lower_body_forms(items, &child, ctx)?
                }
                _ => lower(&raw.body, &child, ctx)?,
            };
            Ok((el, body, raw.ret))
        }
        None => Err(format!("unknown function in map/filter: {}", name)),
    }
}
