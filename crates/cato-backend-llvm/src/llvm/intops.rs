//! Integer binary operators in LLVM: the rendering of
//! `cato_core::hir::eval_int_binop` (the folder's and interpreter's
//! semantics table), with defined behaviour for every input.

use super::*;

/// One integer binary operation at its width — the LLVM rendering of
/// `cato_core::hir::eval_int_binop` (the folder's and interpreter's table).
/// Shift amounts are reduced modulo the width: an LLVM shift by >= width is
/// poison, and Cato defines every shift. Floored `mod` corrects the
/// truncated remainder when its sign differs from the divisor's.
pub(crate) fn emit_int_binop(
    op: BinOp,
    av: &str,
    bv: &str,
    ty: Ty,
    c: &mut FnCtx,
    mdl: &mut Mdl,
) -> String {
    let ll = Mdl::ll_scalar(ty);
    let simple = |c: &mut FnCtx, inst: &str, x: &str, y: &str| {
        let t = c.temp();
        c.line(&format!("  {} = {} {} {}, {}", t, inst, ll, x, y));
        t
    };
    let masked = |c: &mut FnCtx| {
        let t = c.temp();
        c.line(&format!("  {} = and {} {}, {}", t, ll, bv, ty.bits() - 1));
        t
    };
    match op {
        BinOp::Add => simple(c, "add", av, bv),
        BinOp::Sub => simple(c, "sub", av, bv),
        BinOp::Mul => simple(c, "mul", av, bv),
        BinOp::Div | BinOp::Rem | BinOp::Mod => {
            // A zero divisor traps, as in the interpreter (LLVM leaves
            // division by zero undefined). The branch folds away for
            // constant divisors.
            let z = c.temp();
            c.line(&format!("  {} = icmp eq {} {}, 0", z, ll, bv));
            let bad = c.block_name();
            let ok = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", z, bad, ok));
            let l = c.open(&bad);
            c.line(&format!("{}:", l));
            mdl.ensure_decl("declare void @cato_div_by_zero()");
            c.line("  call void @cato_div_by_zero()");
            c.line("  unreachable");
            let l = c.open(&ok);
            c.line(&format!("{}:", l));
            emit_divlike(op, av, bv, ty, c)
        }
        BinOp::BitAnd => simple(c, "and", av, bv),
        BinOp::BitOr => simple(c, "or", av, bv),
        BinOp::BitXor => simple(c, "xor", av, bv),
        BinOp::Shl => {
            let s = masked(c);
            simple(c, "shl", av, &s)
        }
        BinOp::Shr => {
            let s = masked(c);
            simple(c, if ty.is_uint() { "lshr" } else { "ashr" }, av, &s)
        }
    }
}

/// `div`/`rem`/`mod` with a known non-zero divisor. Signed `MIN / -1`
/// overflows (undefined for LLVM's `sdiv`/`srem`): a -1 divisor is
/// replaced by 1 and the quotient negated (wrapping), the remainder is 0 —
/// the folder's and interpreter's `wrapping_div`/`wrapping_rem`.
fn emit_divlike(op: BinOp, av: &str, bv: &str, ty: Ty, c: &mut FnCtx) -> String {
    let ll = Mdl::ll_scalar(ty);
    let bin = |c: &mut FnCtx, inst: &str, x: &str, y: &str| {
        let t = c.temp();
        c.line(&format!("  {} = {} {} {}, {}", t, inst, ll, x, y));
        t
    };
    if ty.is_uint() {
        return bin(c, if op == BinOp::Div { "udiv" } else { "urem" }, av, bv);
    }
    let m1 = c.temp();
    c.line(&format!("  {} = icmp eq {} {}, -1", m1, ll, bv));
    let bs = c.temp();
    c.line(&format!("  {} = select i1 {}, {} 1, {} {}", bs, m1, ll, ll, bv));
    match op {
        BinOp::Div => {
            let q = bin(c, "sdiv", av, &bs);
            let neg = bin(c, "sub", "0", av);
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, {} {}, {} {}", t, m1, ll, neg, ll, q));
            t
        }
        BinOp::Rem => bin(c, "srem", av, &bs),
        _ => {
            // Floored: r = a srem b; if r != 0 && (r ^ b) < 0 then r + b.
            let r = bin(c, "srem", av, &bs);
            let x = bin(c, "xor", &r, bv);
            let neg = c.temp();
            c.line(&format!("  {} = icmp slt {} {}, 0", neg, ll, x));
            let nz = c.temp();
            c.line(&format!("  {} = icmp ne {} {}, 0", nz, ll, r));
            let fix = c.temp();
            c.line(&format!("  {} = and i1 {}, {}", fix, neg, nz));
            let rb = bin(c, "add", &r, bv);
            let t = c.temp();
            c.line(&format!("  {} = select i1 {}, {} {}, {} {}", t, fix, ll, rb, ll, r));
            t
        }
    }
}
