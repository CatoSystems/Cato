//! Numeric widths: narrow vector elements and the integer operators'
//! semantics (one table shared by constant folding and the interpreter;
//! the LLVM backend emits the same operations).

use super::*;

/// The narrow numeric element widths (`i64`/`f64` keep their own `Elem`
/// variants; `Ty` cannot nest in `Elem`, which nests in `Ty`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Narrow {
    I8,
    I16,
    I32,
    U8,
    U16,
    U32,
    U64,
    F32,
}

impl Narrow {
    pub fn ty(self) -> Ty {
        match self {
            Narrow::I8 => Ty::I8,
            Narrow::I16 => Ty::I16,
            Narrow::I32 => Ty::I32,
            Narrow::U8 => Ty::U8,
            Narrow::U16 => Ty::U16,
            Narrow::U32 => Ty::U32,
            Narrow::U64 => Ty::U64,
            Narrow::F32 => Ty::F32,
        }
    }
    pub fn of_ty(t: Ty) -> Option<Narrow> {
        Some(match t {
            Ty::I8 => Narrow::I8,
            Ty::I16 => Narrow::I16,
            Ty::I32 => Narrow::I32,
            Ty::U8 => Narrow::U8,
            Ty::U16 => Narrow::U16,
            Ty::U32 => Narrow::U32,
            Ty::U64 => Narrow::U64,
            Ty::F32 => Narrow::F32,
            _ => return None,
        })
    }
    pub fn bytes(self) -> u32 {
        match self {
            Narrow::I8 | Narrow::U8 => 1,
            Narrow::I16 | Narrow::U16 => 2,
            Narrow::I32 | Narrow::U32 | Narrow::F32 => 4,
            Narrow::U64 => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Narrow::I8 => "i8",
            Narrow::I16 => "i16",
            Narrow::I32 => "i32",
            Narrow::U8 => "u8",
            Narrow::U16 => "u16",
            Narrow::U32 => "u32",
            Narrow::U64 => "u64",
            Narrow::F32 => "f32",
        }
    }
}

impl Ty {
    /// Bit width of an integer type (64 for anything else).
    pub fn bits(self) -> u32 {
        match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 => 32,
            _ => 64,
        }
    }

    /// Reduce an i64 to this integer width's range: sign-extended for
    /// signed widths, zero-extended for unsigned (64-bit widths unchanged).
    pub fn wrap(self, v: i64) -> i64 {
        let bits = self.bits();
        if bits == 64 {
            return v;
        }
        let m = v & ((1i64 << bits) - 1);
        if !self.is_uint() && m >= (1i64 << (bits - 1)) {
            m - (1i64 << bits)
        } else {
            m
        }
    }
}

impl BinOp {
    /// Defined on integer widths only (no float meaning).
    pub fn int_only(self) -> bool {
        matches!(
            self,
            BinOp::Mod | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr
        )
    }
    /// The surface operators, by name.
    pub fn of_name(s: &str) -> Option<BinOp> {
        Some(match s {
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "rem" => BinOp::Rem,
            "mod" => BinOp::Mod,
            "bit-and" => BinOp::BitAnd,
            "bit-or" => BinOp::BitOr,
            "bit-xor" => BinOp::BitXor,
            "bit-shift-left" => BinOp::Shl,
            "bit-shift-right" => BinOp::Shr,
            _ => return None,
        })
    }
}

/// The integer semantics of every binary operator, at width `ty`, over
/// operands already in that width's range (sign-extended for signed,
/// zero-extended for unsigned). The single source of truth for constant
/// folding and the interpreter; the LLVM backend emits the same table.
/// `None`: division or remainder by zero (left to run time).
pub fn eval_int_binop(op: BinOp, x: i64, y: i64, ty: Ty) -> Option<i64> {
    let unsigned = ty.is_uint();
    let shift = (y as u32) & (ty.bits() - 1);
    let v = match op {
        BinOp::Add => x.wrapping_add(y),
        BinOp::Sub => x.wrapping_sub(y),
        BinOp::Mul => x.wrapping_mul(y),
        BinOp::Div if y == 0 => return None,
        BinOp::Div if unsigned => ((x as u64) / (y as u64)) as i64,
        BinOp::Div => x.wrapping_div(y),
        BinOp::Rem | BinOp::Mod if y == 0 => return None,
        BinOp::Rem | BinOp::Mod if unsigned => ((x as u64) % (y as u64)) as i64,
        BinOp::Rem => x.wrapping_rem(y),
        BinOp::Mod => {
            let r = x.wrapping_rem(y);
            if r != 0 && ((r ^ y) < 0) {
                r + y
            } else {
                r
            }
        }
        BinOp::BitAnd => x & y,
        BinOp::BitOr => x | y,
        BinOp::BitXor => x ^ y,
        BinOp::Shl => x.wrapping_shl(shift),
        BinOp::Shr if unsigned => ((x as u64) >> shift) as i64,
        BinOp::Shr => x >> shift,
    };
    Some(ty.wrap(v))
}
