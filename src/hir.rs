use crate::ast::Expr;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    I64,
    F64,
    VecI64,
    Bool,
    /// User-defined ADT, indexed into the program's `AdtDef` table.
    Adt(u32),
}

/// A user-defined algebraic data type: variants with scalar (or other-ADT)
/// payload fields, in declaration order (the order is the runtime tag).
#[derive(Debug, Clone, PartialEq)]
pub struct AdtDef {
    pub name: String,
    pub variants: Vec<(String, Vec<Ty>)>,
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
    Ctor {
        adt: u32,
        variant: u32,
        args: Vec<HExpr>,
    },
    Match {
        scrut: Box<HExpr>,
        arms: Vec<HArm>,
        ty: Ty,
    },
    /// `(get vec idx)` / `(get vec idx default)` — runtime-bounds-checked
    /// element load, emitted branchlessly (select, no basic blocks).
    Get {
        src: Box<HExpr>,
        idx: Box<HExpr>,
        default: Option<Box<HExpr>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HArm {
    /// Variant tag; `None` is the `_` wildcard arm.
    pub variant: Option<u32>,
    pub binders: Vec<String>,
    pub body: HExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HReduce {
    Pipeline {
        combine: BinOp,
        init: HExpr,
        maps: Vec<MapSpec>,
        filters: Vec<FilterSpec>,
        src: HSource,
    },
    Closure {
        init: HExpr,
        src: HSource,
        acc: String,
        el: String,
        el2: Option<String>,
        body: HExpr,
        maps: Vec<MapSpec>,
        filters: Vec<FilterSpec>,
    },
}

/// The source of a fused loop. `Take`/`Drop` adjust the loop bounds, `Range`
/// generates elements without a backing vector, and `Zip` reads two vectors
/// in lockstep (binding a second element variable) — all with zero
/// allocation, fused into the single loop.
#[derive(Debug, Clone, PartialEq)]
pub enum HSource {
    Var(String),
    /// An inline `(vec-i64 ...)` / vector literal as the loop base.
    Lit(Vec<i64>),
    Range(Box<HExpr>, Box<HExpr>),
    Zip(Box<HSource>, Box<HSource>),
    Take(Box<HExpr>, Box<HSource>),
    Drop(Box<HExpr>, Box<HSource>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapSpec {
    pub el: String,
    /// Second element binding when the stage reads a `Zip` source.
    pub el2: Option<String>,
    pub body: HExpr,
    pub el_ty: Ty,
    pub res_ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterSpec {
    pub el: String,
    pub el2: Option<String>,
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
    adts: &'a [AdtDef],
}

impl<'a> Ctx<'a> {
    /// Resolve a constructor name to (adt index, variant index, field types).
    fn lookup_ctor(&self, name: &str) -> Option<(u32, u32, Vec<Ty>)> {
        for (ai, adt) in self.adts.iter().enumerate() {
            for (vi, (vname, fields)) in adt.variants.iter().enumerate() {
                if vname == name {
                    return Some((ai as u32, vi as u32, fields.clone()));
                }
            }
        }
        None
    }
}

pub fn build(prog: &[Expr]) -> Result<Vec<Defn>, String> {
    Ok(build_program(prog)?.1)
}

/// Build a full program: the ADT table (from `deftype` forms) plus every
/// lowered `defn`.
pub fn build_program(prog: &[Expr]) -> Result<(Vec<AdtDef>, Vec<Defn>), String> {
    // Pass 0: collect deftype forms into the ADT table so every defn (in any
    // order) can refer to the types.
    let mut adts: Vec<AdtDef> = Vec::new();
    for form in prog {
        if is_deftype(form) {
            adts.push(parse_deftype(form)?);
        }
    }
    let mut adt_by_name: HashMap<String, u32> = HashMap::new();
    for (i, adt) in adts.iter().enumerate() {
        if adt_by_name.insert(adt.name.clone(), i as u32).is_some() {
            return Err(format!("duplicate deftype {}", adt.name));
        }
    }

    // Pass 1: parse every defn into a raw (un-lowered) form without lowering,
    // so map/filter and calls can resolve any function regardless of order.
    let raws: Vec<RawDefn> = prog
        .iter()
        .filter(|f| !is_deftype(f))
        .map(|f| parse_raw(f, &adt_by_name))
        .collect::<Result<_, _>>()?;
    let raw_by_name: HashMap<&str, &RawDefn> = raws
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();
    let ctx = Ctx {
        raw_by_name: &raw_by_name,
        adts: &adts,
    };

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
    Ok((adts, out))
}

fn is_deftype(form: &Expr) -> bool {
    matches!(
        form,
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "deftype")
    )
}

/// (deftype Name (Variant [field :- ty ...]) ...)
fn parse_deftype(form: &Expr) -> Result<AdtDef, String> {
    let Expr::List(items) = form else {
        return Err("expected (deftype ...)".into());
    };
    if items.len() < 2 {
        return Err("deftype expects a name and at least one variant".into());
    }
    let Expr::LitSym(name) = &items[1] else {
        return Err("deftype name must be a symbol".into());
    };
    let mut variants = Vec::new();
    for v in &items[2..] {
        let Expr::List(vitems) = v else {
            return Err(format!("deftype {} variant must be a list", name));
        };
        let Some(Expr::LitSym(vname)) = vitems.first() else {
            return Err(format!("deftype {} variant name must be a symbol", name));
        };
        let mut fields = Vec::new();
        if vitems.len() > 1 {
            let Expr::Vector(fitems) = &vitems[1] else {
                return Err(format!("variant {} fields must be a vector", vname));
            };
            if vitems.len() > 2 {
                return Err(format!("variant {} takes only a fields vector", vname));
            }
            let mut i = 0;
            while i < fitems.len() {
                let Expr::LitSym(fname) = &fitems[i] else {
                    return Err(format!("variant {} field name must be a symbol", vname));
                };
                let Some(Expr::LitSym(sep)) = fitems.get(i + 1) else {
                    return Err(format!("variant {} field {} expects (name :- ty)", vname, fname));
                };
                if sep != ":-" {
                    return Err(format!("variant {} field {} expects (name :- ty)", vname, fname));
                }
                let Some(Expr::LitSym(ty)) = fitems.get(i + 2) else {
                    return Err(format!("variant {} field {} expects a type", vname, fname));
                };
                fields.push(parse_scalar_ty(ty)?);
                i += 3;
            }
        }
        variants.push((vname.clone(), fields));
    }
    Ok(AdtDef {
        name: name.clone(),
        variants,
    })
}

fn parse_raw(form: &Expr, adt_by_name: &HashMap<String, u32>) -> Result<RawDefn, String> {
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
    let params = parse_params(params, adt_by_name)?;

    let mut body_start = 3;
    let mut ret = Ty::I64;
    if items.len() > body_start + 1 {
        if let Expr::LitSym(sep) = &items[body_start] {
            if sep == ":-" {
                if let Expr::LitSym(ty) = &items[body_start + 1] {
                    ret = parse_ty(ty, adt_by_name)?;
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

fn parse_params(params: &[Expr], adt_by_name: &HashMap<String, u32>) -> Result<Vec<(String, Ty)>, String> {
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
                        out.push((name.clone(), parse_ty(ty, adt_by_name)?));
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

fn parse_ty(s: &str, adt_by_name: &HashMap<String, u32>) -> Result<Ty, String> {
    match s {
        "i64" => Ok(Ty::I64),
        "f64" => Ok(Ty::F64),
        "vec" => Ok(Ty::VecI64),
        "bool" => Ok(Ty::Bool),
        other => match adt_by_name.get(other) {
            Some(&i) => Ok(Ty::Adt(i)),
            None => Err(format!("unknown type annotation '{}'", other)),
        },
    }
}

/// Payload fields are scalars-only in the ADT cut (nested ADTs by value need
/// monomorphization/indirection, later phases).
fn parse_scalar_ty(s: &str) -> Result<Ty, String> {
    match s {
        "i64" => Ok(Ty::I64),
        "f64" => Ok(Ty::F64),
        "bool" => Ok(Ty::Bool),
        other => Err(format!(
            "ADT field type must be i64, f64 or bool, got '{}'",
            other
        )),
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
                "match" => lower_match(items, env, ctx),
                "get" => lower_get(items, env, ctx),
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
                    return promote_bin(op, a, b, env, ctx);
                }
                _ => {
                    // Constructors resolve before user functions.
                    if let Some((adt, variant, fields)) = ctx.lookup_ctor(head) {
                        if items.len() - 1 != fields.len() {
                            return Err(format!(
                                "constructor {} expects {} argument(s), got {}",
                                head,
                                fields.len(),
                                items.len() - 1
                            ));
                        }
                        let mut args = Vec::new();
                        for (it, fty) in items[1..].iter().zip(fields.iter()) {
                            let a = lower(it, env, ctx)?;
                            let at = ty_of(&a, env, ctx);
                            if *fty == Ty::F64 && at == Ty::I64 {
                                args.push(HExpr::ConvF64(Box::new(a)));
                            } else if at != *fty {
                                return Err(format!(
                                    "constructor {} field expects {:?}, got {:?}",
                                    head, fty, at
                                ));
                            } else {
                                args.push(a);
                            }
                        }
                        return Ok(HExpr::Ctor { adt, variant, args });
                    }
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

fn promote_pair(a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> Result<(HExpr, HExpr), String> {
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    if matches!(at, Ty::Adt(_)) || matches!(bt, Ty::Adt(_)) {
        return Err("ADT values cannot be used in arithmetic or comparisons".into());
    }
    let f = at == Ty::F64 || bt == Ty::F64;
    Ok((coerce_f64(a, at, f), coerce_f64(b, bt, f)))
}

fn promote_bin(op: BinOp, a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    let (a, b) = promote_pair(a, b, env, ctx)?;
    Ok(HExpr::Bin(op, Box::new(a), Box::new(b)))
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
    )?;
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
    )?;
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

fn lower_match(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() < 3 {
        return Err("match expects a scrutinee and at least one arm".into());
    }
    let scrut = lower(&items[1], env, ctx)?;
    let Ty::Adt(adt) = ty_of(&scrut, env, ctx) else {
        return Err("match scrutinee must be an ADT value".into());
    };
    let adt_def = &ctx.adts[adt as usize];

    let mut covered = vec![false; adt_def.variants.len()];
    let mut wildcard = false;
    let mut arm_tys: Vec<Ty> = Vec::new();
    let mut arm_exprs: Vec<(Option<u32>, Vec<String>, HExpr)> = Vec::new();

    for arm in &items[2..] {
        let Expr::List(arm_items) = arm else {
            return Err("match arm must be a list".into());
        };
        let Some(Expr::LitSym(head)) = arm_items.first() else {
            return Err("match arm must start with a variant name or _".into());
        };
        if head == "_" {
            if wildcard {
                return Err("match has more than one wildcard arm".into());
            }
            if arm_items.len() != 2 {
                return Err("wildcard arm expects (_ body)".into());
            }
            wildcard = true;
            let body = lower(&arm_items[1], env, ctx)?;
            arm_tys.push(ty_of(&body, env, ctx));
            arm_exprs.push((None, Vec::new(), body));
            continue;
        }
        let Some((vi, (_, fields))) = adt_def
            .variants
            .iter()
            .enumerate()
            .find(|(_, (n, _))| n == head)
            .map(|(vi, v)| (vi, v.clone()))
        else {
            return Err(format!(
                "{} is not a variant of {}",
                head, adt_def.name
            ));
        };
        if covered[vi] {
            return Err(format!("match covers {} more than once", head));
        }
        covered[vi] = true;
        if arm_items.len() != 3 {
            return Err(format!(
                "match arm {} expects ({} [binders...] body)",
                head, head
            ));
        }
        let Expr::Vector(binders) = &arm_items[1] else {
            return Err(format!("match arm {} binders must be a vector", head));
        };
        if binders.len() != fields.len() {
            return Err(format!(
                "variant {} has {} field(s) but the arm binds {}",
                head,
                fields.len(),
                binders.len()
            ));
        }
        let mut bindernames = Vec::new();
        let mut child = env.clone();
        for (b, fty) in binders.iter().zip(fields.iter()) {
            let Expr::LitSym(bn) = b else {
                return Err(format!("match arm {} binder must be a symbol", head));
            };
            child.insert(bn.clone(), *fty);
            bindernames.push(bn.clone());
        }
            let body = lower(&arm_items[2], &child, ctx)?;
            let bty = ty_of(&body, &child, ctx);
            arm_tys.push(bty);
            arm_exprs.push((Some(vi as u32), bindernames, body));
    }

    // Exhaustiveness: every variant covered, or a wildcard arm present.
    if !wildcard {
        let missing: Vec<&str> = adt_def
            .variants
            .iter()
            .zip(covered.iter())
            .filter(|(_, c)| !**c)
            .map(|((n, _), _)| n.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "match on {} does not cover: {} (add arms or a _ wildcard)",
                adt_def.name,
                missing.join(", ")
            ));
        }
    }

    // All arm bodies must agree on the result type (with the if-rule f64
    // promotion). Coerce int arms up when any arm is f64.
    let mut merged = arm_tys[0];
    for t in &arm_tys[1..] {
        if *t == Ty::F64 || merged == Ty::F64 {
            if *t != merged && *t != Ty::I64 && merged != Ty::I64 {
                return Err(format!(
                    "match arm result types differ: {:?} vs {:?}",
                    merged, t
                ));
            }
            merged = Ty::F64;
        } else if *t != merged {
            return Err(format!(
                "match arm result types differ: {:?} vs {:?}",
                merged, t
            ));
        }
    }

    let mut h_arms = Vec::new();
    for ((variant, binders, body), at) in arm_exprs.into_iter().zip(arm_tys.iter()) {
        let body = coerce_f64(body, *at, merged == Ty::F64);
        h_arms.push(HArm {
            variant,
            binders,
            body,
        });
    }
    Ok(HExpr::Match {
        scrut: Box::new(scrut),
        arms: h_arms,
        ty: merged,
    })
}

fn lower_get(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 && items.len() != 4 {
        return Err("get expects (get <vec> <idx>) or (get <vec> <idx> <default>)".into());
    }
    let src = lower(&items[1], env, ctx)?;
    if ty_of(&src, env, ctx) != Ty::VecI64 {
        return Err("get source must be a vector".into());
    }
    let idx = lower(&items[2], env, ctx)?;
    let default = match items.get(3) {
        Some(d) => Some(Box::new(lower(d, env, ctx)?)),
        None => None,
    };
    Ok(HExpr::Get {
        src: Box::new(src),
        idx: Box::new(idx),
        default,
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
        HExpr::Ctor { adt, .. } => Ty::Adt(*adt),
        HExpr::Match { ty, .. } => *ty,
        HExpr::Get { .. } => Ty::I64,
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
                src,
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
            let init = lower(&items[2], env, ctx)?;
            let init_ty = ty_of(&init, env, ctx);
            let (src, maps, filters) = pipeline_src(&items[3], env, ctx)?;
            let zip = matches!(src, HSource::Zip(..));
            let arity = if zip { 3 } else { 2 };
            if params.len() != arity {
                return Err(format!(
                    "reduce fn must bind exactly [acc el{}] here",
                    if zip { " el2" } else { "" }
                ));
            }
            let acc = param_name(&params[0])?;
            let el = param_name(&params[1])?;
            let el2 = if zip {
                Some(param_name(&params[2])?)
            } else {
                None
            };
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
            let mut child = env.clone();
            child.insert(acc.clone(), init_ty);
            child.insert(el.clone(), el_ty);
            if let Some(e2) = &el2 {
                child.insert(e2.clone(), Ty::I64);
            }
            let body = lower(&fnitems[2], &child, ctx)?;
            Ok(HExpr::Reduce(Box::new(HReduce::Closure {
                init,
                src,
                acc,
                el,
                el2,
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

// Peel source wrappers and map/filter stages, inner-most first, returning
// (source, maps, filters). Sources fuse into one loop: a vector variable,
// a `range`, a `zip` of two vector variables, or any of those wrapped in
// `take`/`drop`. Map/filter functions may be inline `(fn ...)` lambdas,
// references to defined functions, or the `even?` builtin.
fn pipeline_src(
    src: &Expr,
    env: &Env,
    ctx: &Ctx,
) -> Result<(HSource, Vec<MapSpec>, Vec<FilterSpec>), String> {
    let Expr::List(items) = src else {
        return Ok((expr_to_source(src, env, ctx)?, Vec::new(), Vec::new()));
    };
    let Some(Expr::LitSym(head)) = items.first() else {
        return Ok((expr_to_source(src, env, ctx)?, Vec::new(), Vec::new()));
    };
    match head.as_str() {
        "map" => {
            if items.len() < 3 {
                return Err("map expects (map <fn> <src>)".into());
            }
            let (inner, mut maps, filters) = pipeline_src(&items[2], env, ctx)?;
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
            let zip = matches!(inner, HSource::Zip(..));
            let (el, el2, body, res_ty) = lambda_spec(&items[1], zip, el_ty, env, ctx)?;
            maps.push(MapSpec {
                el,
                el2,
                body,
                el_ty,
                res_ty,
            });
            Ok((inner, maps, filters))
        }
        "filter" => {
            if items.len() < 3 {
                return Err("filter expects (filter <fn> <src>)".into());
            }
            let (inner, maps, mut filters) = pipeline_src(&items[2], env, ctx)?;
            let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
            let zip = matches!(inner, HSource::Zip(..));
            let (el, el2, body, _) = lambda_spec(&items[1], zip, el_ty, env, ctx)?;
            filters.push(FilterSpec {
                el,
                el2,
                body,
                el_ty,
            });
            Ok((inner, maps, filters))
        }
        "take" => {
            if items.len() != 3 {
                return Err("take expects (take <n> <src>)".into());
            }
            let n = lower(&items[1], env, ctx)?;
            let (inner, maps, filters) = pipeline_src(&items[2], env, ctx)?;
            Ok((HSource::Take(Box::new(n), Box::new(inner)), maps, filters))
        }
        "drop" => {
            if items.len() != 3 {
                return Err("drop expects (drop <n> <src>)".into());
            }
            let n = lower(&items[1], env, ctx)?;
            let (inner, maps, filters) = pipeline_src(&items[2], env, ctx)?;
            Ok((HSource::Drop(Box::new(n), Box::new(inner)), maps, filters))
        }
        "zip" => {
            if items.len() != 3 {
                return Err("zip expects (zip <xs> <ys>)".into());
            }
            let a = vec_source_arg(&items[1], env, ctx)?;
            let b = vec_source_arg(&items[2], env, ctx)?;
            Ok((HSource::Zip(Box::new(a), Box::new(b)), Vec::new(), Vec::new()))
        }
        "range" => {
            match items.len() {
                2 => {
                    let end = lower(&items[1], env, ctx)?;
                    Ok((
                        HSource::Range(Box::new(HExpr::ConstI64(0)), Box::new(end)),
                        Vec::new(),
                        Vec::new(),
                    ))
                }
                3 => {
                    let a = lower(&items[1], env, ctx)?;
                    let b = lower(&items[2], env, ctx)?;
                    Ok((HSource::Range(Box::new(a), Box::new(b)), Vec::new(), Vec::new()))
                }
                n => Err(format!("range expects 1 or 2 arguments, got {}", n - 1)),
            }
        }
        _ => Ok((expr_to_source(src, env, ctx)?, Vec::new(), Vec::new())),
    }
}

/// A plain vector source: a variable bound to a vector or an inline
/// vector literal.
fn expr_to_source(src: &Expr, env: &Env, ctx: &Ctx) -> Result<HSource, String> {
    match src {
        Expr::LitSym(v) => Ok(HSource::Var(v.clone())),
        Expr::Vector(items) if items.iter().all(|e| matches!(e, Expr::LitI64(_))) => {
            Ok(HSource::Lit(vec_lit_values(items)))
        }
        Expr::List(items)
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "vec-i64")
                && items[1..].iter().all(|e| matches!(e, Expr::LitI64(_))) =>
        {
            Ok(HSource::Lit(vec_lit_values(&items[1..])))
        }
        _ => {
            let _ = lower(src, env, ctx);
            Err("reduce source must be a vector variable or literal".into())
        }
    }
}

fn vec_lit_values(items: &[Expr]) -> Vec<i64> {
    items
        .iter()
        .map(|e| if let Expr::LitI64(v) = e { *v } else { unreachable!() })
        .collect()
}

fn vec_source_arg(src: &Expr, _env: &Env, _ctx: &Ctx) -> Result<HSource, String> {
    match src {
        Expr::LitSym(v) => Ok(HSource::Var(v.clone())),
        _ => Err("zip arguments must be vector variables".into()),
    }
}

fn lambda_spec(
    f: &Expr,
    zip: bool,
    el_ty: Ty,
    env: &Env,
    ctx: &Ctx,
) -> Result<(String, Option<String>, HExpr, Ty), String> {
    let arity = if zip { 2 } else { 1 };
    match f {
        Expr::List(items) => {
            // Inline (fn [el] body) or (fn [el el2] body)
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
            if params.len() != arity {
                return Err(format!(
                    "fn must bind exactly {} element param(s) here",
                    arity
                ));
            }
            let el = param_name(&params[0])?;
            let el2 = if zip {
                Some(param_name(&params[1])?)
            } else {
                None
            };
            let mut child = env.clone();
            child.insert(el.clone(), el_ty);
            if let Some(e2) = &el2 {
                child.insert(e2.clone(), Ty::I64);
            }
            let body = lower(&items[2], &child, ctx)?;
            let res_ty = ty_of(&body, &child, ctx);
            Ok((el, el2, body, res_ty))
        }
        Expr::LitSym(name) => resolve_function_ref(name, zip, el_ty, env, ctx),
        _ => Err("map/filter arg must be a function".into()),
    }
}

fn resolve_function_ref(
    name: &str,
    zip: bool,
    el_ty: Ty,
    env: &Env,
    ctx: &Ctx,
) -> Result<(String, Option<String>, HExpr, Ty), String> {
    if name == "even?" {
        if zip {
            return Err("even? cannot be used over a zip source".into());
        }
        return Ok((
            "x".to_string(),
            None,
            HExpr::Call("even?".to_string(), vec![HExpr::Var("x".to_string())]),
            Ty::Bool,
        ));
    }
    match ctx.raw_by_name.get(name) {
        Some(raw) => {
            let arity = if zip { 2 } else { 1 };
            if raw.params.len() != arity {
                return Err(format!(
                    "function {} used in map/filter must have {} param(s)",
                    name, arity
                ));
            }
            let el = raw.params[0].0.clone();
            let el2 = if zip {
                Some(raw.params[1].0.clone())
            } else {
                None
            };
            let mut child = env.clone();
            child.insert(el.clone(), el_ty);
            if let Some(e2) = &el2 {
                child.insert(e2.clone(), Ty::I64);
            }
            let body = match &raw.body {
                Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
                    lower_body_forms(items, &child, ctx)?
                }
                _ => lower(&raw.body, &child, ctx)?,
            };
            Ok((el, el2, body, raw.ret))
        }
        None => Err(format!("unknown function in map/filter: {}", name)),
    }
}
