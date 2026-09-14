//! Constant folding and structural simplification.
//!
//! Folding rules mirror the two backends exactly: integer arithmetic wraps
//! (LLVM plain `add`/`sdiv`; the interpreter's `wrapping_*`), width results
//! are normalized the way the interpreter's `wrap` does, and division folds
//! only for a nonzero constant divisor (`sdiv` by zero is a runtime trap —
//! folding it would erase a panic).

use super::walk::{is_drop_safe, rewrite};
use crate::hir::*;
use crate::Program;
use std::cell::Cell;

pub fn run(program: &mut Program) -> bool {
    let mut changed = false;
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        changed |= fold_body(&mut d.body);
    }
    for lam in program.lambdas.iter_mut() {
        changed |= fold_body(&mut lam.body);
    }
    changed
}

fn fold_body(body: &mut HExpr) -> bool {
    let changed = Cell::new(false);
    let taken = std::mem::replace(body, HExpr::ConstI64(0));
    *body = rewrite(taken, &mut |e| {
        let before = e.clone();
        let folded = fold_one(e);
        if folded != before {
            changed.set(true);
        }
        folded
    });
    changed.get()
}

fn fold_one(e: HExpr) -> HExpr {
    match e {
        HExpr::Bin(op, a, b) => {
            // `Bin` covers i64 arithmetic and the historical i64→f64
            // auto-promotion, so try both constant shapes.
            if let (Some(x), Some(y)) = (as_i64(&a), as_i64(&b)) {
                if let Some(v) = fold_i64(op, x, y) {
                    return HExpr::ConstI64(v);
                }
                return HExpr::Bin(op, a, b);
            }
            if let (Some(x), Some(y)) = (as_f64(&a), as_f64(&b)) {
                if let Some(v) = fold_f64(op, x, y) {
                    return HExpr::ConstF64(v);
                }
            }
            HExpr::Bin(op, a, b)
        }
        HExpr::BinW(op, a, b, ty) => match (as_i64(&a), as_i64(&b)) {
            (Some(x), Some(y)) if ty.is_int() => {
                let v = match op {
                    BinOp::Add => x.wrapping_add(y),
                    BinOp::Sub => x.wrapping_sub(y),
                    BinOp::Mul => x.wrapping_mul(y),
                    BinOp::Div => {
                        if y == 0 {
                            return HExpr::BinW(op, a, b, ty);
                        }
                        x.wrapping_div(y)
                    }
                };
                HExpr::ConstI64(wrap_bits(v, ty))
            }
            _ => HExpr::BinW(op, a, b, ty),
        },
        HExpr::BinF(op, a, b) => {
            // f32 arithmetic in f32 precision (both operands are f32-typed;
            // constants carry their value as f64).
            match (as_f64(&a), as_f64(&b)) {
                (Some(x), Some(y)) => {
                    let (x, y) = (x as f32, y as f32);
                    // IEEE arithmetic; only finite results fold so neither
                    // backend ever sees an unusual constant.
                    let v = match op {
                        BinOp::Add => x + y,
                        BinOp::Sub => x - y,
                        BinOp::Mul => x * y,
                        BinOp::Div => x / y,
                    };
                    if v.is_finite() {
                        HExpr::ConstF64(v as f64)
                    } else {
                        HExpr::BinF(op, a, b)
                    }
                }
                _ => HExpr::BinF(op, a, b),
            }
        }
        HExpr::ConvF64(inner) => match as_i64(&inner) {
            Some(v) => HExpr::ConstF64(v as f64),
            None => HExpr::ConvF64(inner),
        },
        HExpr::ICast(inner, ty) => match (&*inner, ty) {
            (HExpr::ConstI64(v), t) if t.is_int() => HExpr::ConstI64(wrap_bits(*v, t)),
            _ => HExpr::ICast(inner, ty),
        },
        HExpr::Cmp(op, a, b, is_f64) => {
            let r = if is_f64 {
                match (as_f64(&a), as_f64(&b)) {
                    (Some(x), Some(y)) if !x.is_nan() && !y.is_nan() => apply_cmp(op, x == y, x < y, x > y, x <= y, x >= y),
                    _ => return HExpr::Cmp(op, a, b, is_f64),
                }
            } else {
                match (as_i64(&a), as_i64(&b)) {
                    (Some(x), Some(y)) => apply_cmp(op, x == y, x < y, x > y, x <= y, x >= y),
                    _ => return HExpr::Cmp(op, a, b, is_f64),
                }
            };
            const_bool_node(r)
        }
        HExpr::CmpW(op, a, b, ty) => match (as_i64(&a), as_i64(&b), ty.is_float()) {
            (Some(x), Some(y), false) => {
                // Width-normalized values: plain i64 compare is exact except
                // for 64-bit unsigned ordering (bit patterns invert order).
                let ordered_ok = !ty.is_uint() || ty.int_bits() < 64;
                if op == CmpOp::Eq {
                    const_bool_node(x == y)
                } else if op == CmpOp::Ne {
                    const_bool_node(x != y)
                } else if ordered_ok {
                    const_bool_node(apply_cmp(op, x == y, x < y, x > y, x <= y, x >= y))
                } else {
                    HExpr::CmpW(op, a, b, ty)
                }
            }
            _ => HExpr::CmpW(op, a, b, ty),
        },
        HExpr::Not(inner) => match const_bool(&inner) {
            Some(b) => const_bool_node(!b),
            None => HExpr::Not(inner),
        },
        HExpr::Abs(inner, ty) => match (&*inner, &ty) {
            (HExpr::ConstI64(v), t) if t.is_int() && *v != i64::MIN => {
                HExpr::ConstI64(wrap_bits(v.abs(), *t))
            }
            (HExpr::ConstF64(v), Ty::F64) if v.is_finite() => HExpr::ConstF64(v.abs()),
            _ => HExpr::Abs(inner, ty),
        },
        HExpr::Min(a, b, ty) => fold_minmax(a, b, ty, true),
        HExpr::Max(a, b, ty) => fold_minmax(a, b, ty, false),
        HExpr::If(c, t, f, ty) => {
            // `Not` conds swap the branches without needing a bool constant
            // (HIR has none; both backends branch on computed booleans).
            if let HExpr::Not(inner) = &*c {
                return HExpr::If(inner.clone(), f, t, ty);
            }
            match const_bool(&c) {
                Some(true) => *t,
                Some(false) => *f,
                None => HExpr::If(c, t, f, ty),
            }
        }
        HExpr::Count(inner) => match &*inner {
            HExpr::VecLit(VLit::I64(vs)) => HExpr::ConstI64(vs.len() as i64),
            HExpr::VecLit(VLit::F64(vs)) => HExpr::ConstI64(vs.len() as i64),
            HExpr::StrLit(s) => HExpr::ConstI64(s.len() as i64),
            _ => HExpr::Count(inner),
        },
        HExpr::Get {
            src,
            idx,
            default,
            ty,
        } => match (&*src, as_i64(&idx)) {
            (HExpr::VecLit(VLit::I64(vs)), Some(i)) => {
                if i >= 0 && (i as usize) < vs.len() {
                    // Dropping an evaluated-by-default expression could
                    // remove work only if it had effects; require pure.
                    let default_ok = default
                        .as_deref()
                        .map(is_drop_safe)
                        .unwrap_or(true);
                    if default_ok {
                        HExpr::ConstI64(vs[i as usize])
                    } else {
                        HExpr::Get { src, idx, default, ty }
                    }
                } else if let Some(d) = default {
                    *d
                } else {
                    HExpr::Get { src, idx, default, ty }
                }
            }
            (HExpr::VecLit(VLit::F64(vs)), Some(i)) => {
                if i >= 0 && (i as usize) < vs.len() {
                    let default_ok = default
                        .as_deref()
                        .map(is_drop_safe)
                        .unwrap_or(true);
                    if default_ok {
                        HExpr::ConstF64(vs[i as usize])
                    } else {
                        HExpr::Get { src, idx, default, ty }
                    }
                } else if let Some(d) = default {
                    *d
                } else {
                    HExpr::Get { src, idx, default, ty }
                }
            }
            _ => HExpr::Get { src, idx, default, ty },
        },
        HExpr::StrCat(args) => {
            let mut all = String::new();
            for a in &args {
                match a {
                    HExpr::StrLit(s) => all.push_str(s),
                    _ => return HExpr::StrCat(args),
                }
            }
            HExpr::StrLit(all)
        }
        HExpr::StrCmp(p, a, b) => match (&*a, &*b) {
            (HExpr::StrLit(x), HExpr::StrLit(y)) => const_bool_node((x == y) == p),
            _ => HExpr::StrCmp(p, a, b),
        },
        HExpr::SetContains { s, x } => match (&*s, as_i64(&x)) {
            (HExpr::SetLit(vs), Some(k)) => {
                // Set literals are sorted and deduped at lowering.
                const_bool_node(vs.iter().any(|v| as_i64(v) == Some(k)))
            }
            _ => HExpr::SetContains { s, x },
        },
        HExpr::MapGet { src, key, default } => {
            if let (HExpr::MapLit(pairs), Some(k)) = (&*src, as_i64(&key)) {
                // Last match wins; fold only when the key is unambiguous.
                let hits: Vec<&HExpr> = pairs
                    .iter()
                    .filter(|(pk, _)| as_i64(pk) == Some(k))
                    .map(|(_, pv)| pv)
                    .collect();
                if hits.len() == 1 {
                    if let Some(v) = as_i64(hits[0]) {
                        return HExpr::ConstI64(v);
                    }
                } else if hits.is_empty() {
                    if let Some(d) = default {
                        return *d;
                    }
                }
            }
            HExpr::MapGet { src, key, default }
        }
        HExpr::Subs { s, start, end } => {
            let folded = match (&*s, as_i64(&start), as_i64(&end)) {
                (HExpr::StrLit(s), Some(a), Some(b))
                    if a >= 0 && b >= 0 && a <= b && (b as usize) <= s.len() =>
                {
                    s.get(a as usize..b as usize).map(|sub| HExpr::StrLit(sub.to_string()))
                }
                _ => None,
            };
            folded.unwrap_or_else(move || HExpr::Subs { s, start, end })
        }
        HExpr::Call(name, args) => {
            if name == "even?" {
                if let [HExpr::ConstI64(v)] = &args[..] {
                    return const_bool_node(v % 2 == 0);
                }
            }
            HExpr::Call(name, args)
        }
        other => other,
    }
}

/// i64 folding with backend-exact semantics: wrapping ops, division only
/// when it cannot trap or be UB (nonzero divisor; `MIN / -1` skipped).
fn fold_i64(op: BinOp, x: i64, y: i64) -> Option<i64> {
    match op {
        BinOp::Add => Some(x.wrapping_add(y)),
        BinOp::Sub => Some(x.wrapping_sub(y)),
        BinOp::Mul => Some(x.wrapping_mul(y)),
        BinOp::Div => {
            if y == 0 || (x == i64::MIN && y == -1) {
                None
            } else {
                Some(x.wrapping_div(y))
            }
        }
    }
}

/// f64 folding; only well-defined finite results are folded so no backend
/// ever sees an unusual floating-point constant.
fn fold_f64(op: BinOp, x: f64, y: f64) -> Option<f64> {
    let v = match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => x / y,
    };
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

fn fold_minmax(a: Box<HExpr>, b: Box<HExpr>, ty: Ty, is_min: bool) -> HExpr {
    match (as_f64(&a), as_f64(&b), ty) {
        (Some(x), Some(y), t) if t == Ty::F64 => {
            if x.is_nan() || y.is_nan() {
                return HExpr::Min(a, b, ty).pick_max(!is_min);
            }
            let pick_a = if is_min { x < y } else { x > y };
            if pick_a {
                *a
            } else {
                *b
            }
        }
        _ => match (as_i64(&a), as_i64(&b)) {
            (Some(x), Some(y)) if ty.is_int() => {
                let pick_a = if is_min { x < y } else { x > y };
                if pick_a {
                    *a
                } else {
                    *b
                }
            }
            _ => HExpr::Min(a, b, ty).pick_max(!is_min),
        },
    }
}

impl HExpr {
    fn pick_max(self, is_max: bool) -> HExpr {
        match (self, is_max) {
            (HExpr::Min(a, b, t), true) => HExpr::Max(a, b, t),
            (HExpr::Max(a, b, t), false) => HExpr::Min(a, b, t),
            (other, _) => other,
        }
    }
}

fn as_i64(e: &HExpr) -> Option<i64> {
    match e {
        HExpr::ConstI64(v) => Some(*v),
        _ => None,
    }
}

fn as_f64(e: &HExpr) -> Option<f64> {
    match e {
        HExpr::ConstF64(v) => Some(*v),
        _ => None,
    }
}

/// HIR has no boolean constant; a folded `true`/`false` is the comparison
/// of two constants, which every consumer (both backends' branch emitters,
/// further folding rounds) understands.
fn const_bool_node(b: bool) -> HExpr {
    HExpr::Cmp(
        if b { CmpOp::Eq } else { CmpOp::Ne },
        Box::new(HExpr::ConstI64(1)),
        Box::new(HExpr::ConstI64(1)),
        false,
    )
}

fn apply_cmp(op: CmpOp, eq: bool, lt: bool, gt: bool, le: bool, ge: bool) -> bool {
    match op {
        CmpOp::Eq => eq,
        CmpOp::Ne => !eq,
        CmpOp::Lt => lt,
        CmpOp::Gt => gt,
        CmpOp::Le => le,
        CmpOp::Ge => ge,
    }
}

/// Normalize a (possibly out-of-range) i64 into `ty`'s value range; mirrors
/// the interpreter's `wrap` (LLVM trunc/sext semantics).
fn wrap_bits(v: i64, ty: Ty) -> i64 {
    match ty {
        Ty::I8 | Ty::U8 => {
            let m = v & 0xFF;
            if ty == Ty::I8 && m >= 0x80 {
                m - 0x100
            } else {
                m
            }
        }
        Ty::I16 | Ty::U16 => {
            let m = v & 0xFFFF;
            if ty == Ty::I16 && m >= 0x8000 {
                m - 0x1_0000
            } else {
                m
            }
        }
        Ty::I32 | Ty::U32 => {
            let m = v & 0xFFFF_FFFF;
            if ty == Ty::I32 && m >= 0x8000_0000 {
                m - 0x1_0000_0000
            } else {
                m
            }
        }
        _ => v,
    }
}

/// Structurally evaluate a boolean expression to a constant, if possible.
fn const_bool(e: &HExpr) -> Option<bool> {
    match e {
        HExpr::Cmp(op, a, b, is_f64) => {
            if *is_f64 {
                let (x, y) = (as_f64(a)?, as_f64(b)?);
                if x.is_nan() || y.is_nan() {
                    return None;
                }
                Some(apply_cmp(*op, x == y, x < y, x > y, x <= y, x >= y))
            } else {
                let (x, y) = (as_i64(a)?, as_i64(b)?);
                Some(apply_cmp(*op, x == y, x < y, x > y, x <= y, x >= y))
            }
        }
        HExpr::CmpW(op, a, b, ty) if !ty.is_float() => {
            let (x, y) = (as_i64(a)?, as_i64(b)?);
            let ordered_ok = !ty.is_uint() || ty.int_bits() < 64;
            if !ordered_ok && !matches!(op, CmpOp::Eq | CmpOp::Ne) {
                return None;
            }
            Some(apply_cmp(*op, x == y, x < y, x > y, x <= y, x >= y))
        }
        HExpr::Not(inner) => Some(!const_bool(inner)?),
        HExpr::And(a, b) => {
            // Short-circuit: a false LHS forces false without knowing b.
            match const_bool(a) {
                Some(false) => Some(false),
                _ => {
                    let (x, y) = (const_bool(a)?, const_bool(b)?);
                    Some(x && y)
                }
            }
        }
        HExpr::Or(a, b) => match const_bool(a) {
            Some(true) => Some(true),
            _ => {
                let (x, y) = (const_bool(a)?, const_bool(b)?);
                Some(x || y)
            }
        },
        HExpr::StrCmp(p, a, b) => {
            let (x, y) = (as_str(a)?, as_str(b)?);
            Some((x == y) == *p)
        }
        HExpr::SetContains { s, x } => {
            let k = as_i64(x)?;
            let HExpr::SetLit(vs) = &**s else { return None };
            Some(vs.iter().any(|v| as_i64(v) == Some(k)))
        }
        HExpr::Call(name, args) if name == "even?" => {
            let v = match args.first() {
                Some(HExpr::ConstI64(v)) => *v,
                _ => return None,
            };
            Some(v % 2 == 0)
        }
        _ => None,
    }
}

fn as_str(e: &HExpr) -> Option<&str> {
    match e {
        HExpr::StrLit(s) => Some(s),
        _ => None,
    }
}
