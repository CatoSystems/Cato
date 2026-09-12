//! Typed HIR: types, expressions, definitions, plus the build entry points.

use crate::ast::Expr;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    I64,
    /// 32-bit signed integer (narrow width slice).
    I32,
    F64,
    /// Vector with an element type: `(Vector i64)`, `(Vector f64)`.
    Vec(Elem),
    Bool,
    /// Immutable string slice (static constant data; no heap).
    Str,
    /// i64-keyed map of i64 values: a sorted flat array of 16-byte pairs on
    /// the heap (immutable snapshots; assoc copies).
    Map,
    /// i64 set: a sorted flat array of i64 on the heap (immutable
    /// snapshots; conj/disj copy).
    Set,
    /// User-defined ADT, indexed into the program's `AdtDef` table.
    Adt(u32),
    /// Type parameter in a generic defn's annotations, indexed into the
    /// defn's `ty_params` list. Never survives monomorphization.
    Param(u32),
}

/// Vector element types of the monomorphization slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    I64,
    F64,
    /// `(Vector str)` — an array of heap string pointers.
    Str,
    /// `(Vector T)` in a generic template.
    Param(u32),
}

impl Elem {
    pub fn is_param(self) -> bool {
        matches!(self, Elem::Param(_))
    }
}

pub const VEC_I64: Ty = Ty::Vec(Elem::I64);
pub const VEC_F64: Ty = Ty::Vec(Elem::F64);

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
    VecLit(VLit),
    /// A string literal; codegen mints a private `[N x i8]` constant.
    StrLit(String),
    /// `(conj v x)` — a fresh heap vector with x appended (v is copied).
    Conj {
        v: Box<HExpr>,
        x: Box<HExpr>,
        elem: Elem,
    },
    /// `(subvec v start end)` — a fresh heap slice copy, bounds-clamped.
    Subvec {
        v: Box<HExpr>,
        start: Box<HExpr>,
        end: Box<HExpr>,
        elem: Elem,
    },
    /// `(for [x src] body)` — a fresh heap vector of the body results.
    For {
        el: String,
        src: Box<HExpr>,
        elem: Elem,
        body: Box<HExpr>,
        res_elem: Elem,
    },
    /// i32 arithmetic: `(+ a b)` over i32 operands (wrapping).
    BinW(BinOp, Box<HExpr>, Box<HExpr>, Ty),
    /// i32 comparisons.
    CmpW(CmpOp, Box<HExpr>, Box<HExpr>),
    /// `(i32 e)` / `(i64 e)` — explicit widening/narrowing casts.
    ICast(Box<HExpr>, Ty),
    /// `{k v ...}` — a map literal; constant keys are sorted at compile time.
    MapLit(Vec<(HExpr, HExpr)>),
    /// `#{x ...}` — a set literal; constant elements are sorted and deduped
    /// at compile time.
    SetLit(Vec<HExpr>),
    /// `(conj s x)` on a set — a fresh set with x added (dedup).
    SetConj {
        s: Box<HExpr>,
        x: Box<HExpr>,
    },
    /// `(disj s x)` — a fresh set without x.
    SetDisj {
        s: Box<HExpr>,
        x: Box<HExpr>,
    },
    /// `(contains? s x)` — membership test.
    SetContains {
        s: Box<HExpr>,
        x: Box<HExpr>,
    },
    /// `(subs s start end)` — a fresh heap string holding the byte range.
    Subs {
        s: Box<HExpr>,
        start: Box<HExpr>,
        end: Box<HExpr>,
    },
    /// `(split s sep)` — a fresh heap vector of strings.
    Split {
        s: Box<HExpr>,
        sep: Box<HExpr>,
    },
    /// `(join sep xs)` — a fresh heap string joining a (Vector str).
    Join {
        sep: Box<HExpr>,
        xs: Box<HExpr>,
    },
    /// `(get m k [default])` on a map: last match wins (assoc replaces).
    MapGet {
        src: Box<HExpr>,
        key: Box<HExpr>,
        default: Option<Box<HExpr>>,
    },
    /// `(assoc m k v)` — a fresh map with the binding added/replaced.
    MapAssoc {
        m: Box<HExpr>,
        k: Box<HExpr>,
        v: Box<HExpr>,
    },
    /// `(str a b ...)` — a fresh heap string concatenation.
    StrCat(Vec<HExpr>),
    /// `(= s1 s2)` on strings: content comparison via the runtime.
    /// The bool is the expected polarity (true for `=`).
    StrCmp(bool, Box<HExpr>, Box<HExpr>),
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
    /// `(clone x)` — an explicit copy of a non-Copy value; the source stays
    /// usable. For ADTs this materializes a fresh stack slot.
    Clone(Box<HExpr>),
    /// `(drop x)` — releases a non-Copy binding (a no-op without a heap),
    /// marking it moved: using it afterwards is a compile error.
    DropStmt(Box<HExpr>),
    /// `(get vec idx)` / `(get vec idx default)` — runtime-bounds-checked
    /// element load, emitted branchlessly (select, no basic blocks).
    Get {
        src: Box<HExpr>,
        idx: Box<HExpr>,
        default: Option<Box<HExpr>>,
        ty: Ty,
    },
}

/// Typed vector literal contents (element type lives in `Ty::Vec`).
#[derive(Debug, Clone, PartialEq)]
pub enum VLit {
    I64(Vec<i64>),
    F64(Vec<f64>),
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
    /// An inline `(vec-i64 ...)` / `(vec-f64 ...)` literal as the loop base.
    Lit(VLit),
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
    pub el2_ty: Ty,
    pub body: HExpr,
    pub el_ty: Ty,
    pub res_ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterSpec {
    pub el: String,
    pub el2: Option<String>,
    pub el2_ty: Ty,
    pub body: HExpr,
    pub el_ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Defn {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    pub body: HExpr,
    /// Borrow kind per param (same length as `params`).
    pub borrows: Vec<Borrow>,
    /// FFI declaration: emitted as an LLVM `declare`, not a `define`.
    pub is_extern: bool,
}

type Env = HashMap<String, Ty>;

pub(crate) struct RawDefn {
    pub(crate) name: String,
    pub(crate) params: Vec<(String, Ty)>,
    pub(crate) ret: Ty,
    pub(crate) borrows: Vec<Borrow>,
    /// Type parameter names, in declaration order (indexed by `Ty::Param`).
    /// Empty for concrete defns.
    pub(crate) ty_params: Vec<String>,
    /// FFI declaration: no body, the symbol is a C function.
    pub(crate) is_extern: bool,
    pub(crate) body: Expr,
}

/// Monomorphization state shared across lowering: instantiated generics are
/// specialized once per concrete call-site type and appended to the program.
#[derive(Default)]
pub(crate) struct InstState {
    /// mangled name -> instantiated return type (also the instantiation key)
    pub(crate) cache: HashMap<String, Ty>,
    pub(crate) extra: Vec<Defn>,
}

pub(crate) struct Ctx<'a> {
    pub(crate) raw_by_name: &'a HashMap<&'a str, &'a RawDefn>,
    pub(crate) adts: &'a [AdtDef],
    pub(crate) inst: RefCell<InstState>,
}

pub fn build(prog: &[Expr]) -> Result<Vec<Defn>, String> {
    Ok(build_program(prog)?.1)
}

/// Build a full program: the ADT table (from `deftype` forms) plus every
/// lowered `defn`.
mod build;
mod ctl;
mod forms;
mod lower;
mod match_lower;
mod mono;
mod ownership;
mod parse;
mod pipeline;
mod stagefn;

pub use build::{build_program, build_program_located};
pub(crate) use ctl::*;
pub(crate) use forms::*;
pub(crate) use lower::*;
pub(crate) use match_lower::*;
pub(crate) use mono::*;
pub(crate) use ownership::*;
pub(crate) use parse::*;
pub(crate) use pipeline::*;
pub(crate) use stagefn::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Borrow {
    /// Takes the value (moves it at the call site).
    Owned,
    /// Immutable borrow: the caller keeps the value.
    Shared,
    /// Exclusive borrow: the caller keeps the value, but no other borrow of
    /// the same binding may appear in the same call.
    Exclusive,
}

impl Borrow {
    pub fn is_owned(self) -> bool {
        self == Borrow::Owned
    }
}

/// Does this expression fork into basic blocks? (codegen-consistent)

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


