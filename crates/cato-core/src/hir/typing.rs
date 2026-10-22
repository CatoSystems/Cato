//! Typing at call sites and literals.
//!
//! A numeric literal takes the width its context expects when it is
//! exactly representable there (a call argument, the other operand of an
//! arithmetic or comparison, a vector element), and a call to a concrete
//! function must supply exactly its parameters at their declared types.

use super::*;

/// Does an integer literal fit this integer width?
pub(crate) fn int_fits(v: i64, t: Ty) -> bool {
    match t {
        Ty::I64 => true,
        Ty::I32 => i32::try_from(v).is_ok(),
        Ty::I16 => i16::try_from(v).is_ok(),
        Ty::I8 => i8::try_from(v).is_ok(),
        Ty::U32 => u32::try_from(v).is_ok(),
        Ty::U16 => u16::try_from(v).is_ok(),
        Ty::U8 => u8::try_from(v).is_ok(),
        Ty::U64 | Ty::Usize => v >= 0,
        _ => false,
    }
}

/// Contextual literal typing: a numeric literal takes the width its
/// context expects — `(f 5)` for an `i32` parameter, `(+ x 1)` for an `u8`
/// `x`, `(conj bytes 7)` — when the value is exactly representable there.
/// `None`: not a literal, or nothing to change (the literal keeps its own
/// type, and the ordinary width rules decide). `Some(Err)`: the literal
/// does not fit the expected width — a diagnostic, never a silent wrap.
/// A narrow literal is a cast of the constant, folded by the backends.
pub(crate) fn retype_literal(e: &HExpr, want: Ty) -> Option<Result<HExpr, String>> {
    const F64_EXACT: i64 = 1 << 53;
    const F32_EXACT: i64 = 1 << 24;
    match e {
        HExpr::ConstI64(v) if want.is_int() && want != Ty::I64 => Some(if int_fits(*v, want) {
            Ok(HExpr::ICast(Box::new(HExpr::ConstI64(*v)), Ty::I64, want))
        } else {
            Err(format!(
                "integer literal {} does not fit {} (mixed widths: cast a wider value explicitly)",
                v,
                format!("{:?}", want).to_lowercase()
            ))
        }),
        HExpr::ConstI64(v) if want == Ty::F64 && v.unsigned_abs() <= F64_EXACT as u64 => {
            Some(Ok(HExpr::ConstF64(*v as f64)))
        }
        HExpr::ConstI64(v) if want == Ty::F32 && v.unsigned_abs() <= F32_EXACT as u64 => Some(Ok(
            HExpr::ICast(Box::new(HExpr::ConstF64(*v as f64)), Ty::F64, Ty::F32),
        )),
        HExpr::ConstF64(v) if want == Ty::F32 => Some(Ok(HExpr::ICast(
            Box::new(HExpr::ConstF64(*v)),
            Ty::F64,
            Ty::F32,
        ))),
        _ => None,
    }
}

/// `retype_literal`, keeping `e` unchanged when it does not apply.
pub(crate) fn fit_literal(e: HExpr, want: Ty) -> Result<HExpr, String> {
    match retype_literal(&e, want) {
        Some(r) => r,
        None => Ok(e),
    }
}

/// A literal operand adopts the width of the other operand.
pub(crate) fn fit_operands(
    a: HExpr,
    b: HExpr,
    env: &Env,
    ctx: &Ctx,
) -> Result<(HExpr, HExpr), String> {
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    if at == bt || !at.is_numeric() || !bt.is_numeric() {
        return Ok((a, b));
    }
    if let Some(r) = retype_literal(&a, bt) {
        return Ok((r?, b));
    }
    if let Some(r) = retype_literal(&b, at) {
        return Ok((a, r?));
    }
    Ok((a, b))
}

/// A vector literal at a narrow element width; every value must fit.
pub(crate) fn narrow_vec_lit(items: &[Expr], n: Narrow) -> Result<HExpr, String> {
    if n == Narrow::F32 {
        let mut vs = Vec::new();
        for e in items {
            match e {
                Expr::LitF64(v) => vs.push(*v),
                Expr::LitI64(v) => vs.push(*v as f64),
                _ => return Err("a (Vector f32) literal takes numeric literals".into()),
            }
        }
        return Ok(HExpr::VecLit(VLit::F32(vs)));
    }
    let mut vs = Vec::new();
    for e in items {
        match e {
            Expr::LitI64(v) if int_fits(*v, n.ty()) => vs.push(*v),
            Expr::LitI64(v) => {
                return Err(format!(
                    "integer literal {} does not fit {} (a (Vector {}) element)",
                    v,
                    n.name(),
                    n.name()
                ));
            }
            _ => {
                return Err(format!(
                    "a (Vector {}) literal takes integer literals",
                    n.name()
                ));
            }
        }
    }
    Ok(HExpr::VecLit(VLit::N(n, vs)))
}

/// A call to a concrete function must supply exactly its parameters, each
/// of its declared type (after contextual literal typing). Before this
/// check, `(f 5)` against a two-parameter `f` compiled and read an
/// undefined register, and a mistyped argument reached the backend.
pub(crate) fn check_call_args(
    head: &str,
    params: &[Ty],
    args: &[HExpr],
    env: &Env,
    ctx: &Ctx,
) -> Result<(), String> {
    if params.len() != args.len() {
        return Err(format!(
            "{} expects exactly {} argument{}, got {}",
            head,
            params.len(),
            if params.len() == 1 { "" } else { "s" },
            args.len()
        ));
    }
    for (k, (p, a)) in params.iter().zip(args.iter()).enumerate() {
        let at = ty_of(a, env, ctx);
        // `[]` has no elements to type: it is any vector (same header).
        let empty_vec =
            matches!(a, HExpr::VecLit(VLit::I64(v)) if v.is_empty()) && matches!(p, Ty::Vec(_));
        if at != *p && !empty_vec {
            return Err(format!(
                "{}: argument {} must be {}, got {}",
                head,
                k + 1,
                ty_name(*p, ctx),
                ty_name(at, ctx)
            ));
        }
    }
    Ok(())
}

/// A type as the programmer wrote it, for diagnostics.
pub(crate) fn ty_name(t: Ty, ctx: &Ctx) -> String {
    match t {
        Ty::I64 => "i64".into(),
        Ty::I32 => "i32".into(),
        Ty::I16 => "i16".into(),
        Ty::I8 => "i8".into(),
        Ty::U64 => "u64".into(),
        Ty::U32 => "u32".into(),
        Ty::U16 => "u16".into(),
        Ty::U8 => "u8".into(),
        Ty::Usize => "usize".into(),
        Ty::F64 => "f64".into(),
        Ty::F32 => "f32".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "str".into(),
        Ty::Map => "map".into(),
        Ty::Set => "set".into(),
        Ty::Vec(Elem::I64) => "(Vector i64)".into(),
        Ty::Vec(Elem::F64) => "(Vector f64)".into(),
        Ty::Vec(Elem::Str) => "(Vector str)".into(),
        Ty::Adt(i) => ctx
            .adts
            .get(i as usize)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("{:?}", t)),
        other => format!("{:?}", other),
    }
}
