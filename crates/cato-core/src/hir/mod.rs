//! Typed HIR: types, expressions, definitions, plus the build entry points.

use crate::ast::Expr;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    I64,
    /// 32-bit signed integer (narrow width slice).
    I32,
    I8,
    I16,
    U8,
    U16,
    U32,
    U64,
    /// Unsigned machine word (64-bit on this target).
    Usize,
    F64,
    /// 32-bit float.
    F32,
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
    /// First-class closure type, indexed into the program's signature table
    /// `(param tys, ret ty)` — structural, deduped at lowering.
    Fn(u32),
    /// Type parameter in a generic defn's annotations, indexed into the
    /// defn's `ty_params` list. Never survives monomorphization.
    Param(u32),
}

/// Vector element types of the monomorphization slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Elem {
    I64,
    F64,
    /// `(Vector str)` — an array of heap string pointers.
    Str,
    /// `(Vector T)` in a generic template.
    Param(u32),
    /// A narrow numeric element stored at its own width: `(Vector u8)` is
    /// one byte per element, not eight. Reads yield the element type.
    N(Narrow),
}

impl Elem {
    pub fn is_param(self) -> bool {
        matches!(self, Elem::Param(_))
    }
    /// The element's value type (`None` for an unresolved parameter).
    pub fn ty(self) -> Option<Ty> {
        match self {
            Elem::I64 => Some(Ty::I64),
            Elem::F64 => Some(Ty::F64),
            Elem::Str => Some(Ty::Str),
            Elem::N(n) => Some(n.ty()),
            Elem::Param(_) => None,
        }
    }
    /// The element kind storing values of type `t`, if vectors hold it.
    pub fn of_ty(t: Ty) -> Option<Elem> {
        match t {
            Ty::I64 => Some(Elem::I64),
            Ty::F64 => Some(Elem::F64),
            Ty::Str => Some(Elem::Str),
            other => Narrow::of_ty(other).map(Elem::N),
        }
    }
    /// The element type named in an annotation (`i64`, `u8`, `str`, ...).
    pub fn of_name(s: &str) -> Option<Elem> {
        Some(match s {
            "i64" => Elem::I64,
            "f64" => Elem::F64,
            "str" => Elem::Str,
            "i8" => Elem::N(Narrow::I8),
            "i16" => Elem::N(Narrow::I16),
            "i32" => Elem::N(Narrow::I32),
            "u8" => Elem::N(Narrow::U8),
            "u16" => Elem::N(Narrow::U16),
            "u32" => Elem::N(Narrow::U32),
            "u64" => Elem::N(Narrow::U64),
            "f32" => Elem::N(Narrow::F32),
            _ => return None,
        })
    }
    /// Bytes per element in the buffer.
    pub fn bytes(self) -> u32 {
        match self {
            Elem::N(n) => n.bytes(),
            _ => 8,
        }
    }
}

impl Ty {
    pub fn is_int(self) -> bool {
        matches!(
            self,
            Ty::I64 | Ty::I32 | Ty::I8 | Ty::I16 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize
        )
    }

    pub fn is_uint(self) -> bool {
        matches!(self, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize)
    }

    pub fn is_float(self) -> bool {
        matches!(self, Ty::F64 | Ty::F32)
    }

    pub fn is_numeric(self) -> bool {
        self.is_int() || self.is_float()
    }

    /// Copy semantics: assignment/argument passing copies instead of moves.
    /// Strings are Copy (aliasing); boxed ADTs are not (unique owner).
    pub fn is_copy_ty(self) -> bool {
        !matches!(
            self,
            Ty::Adt(_) | Ty::Vec(_) | Ty::Map | Ty::Set | Ty::Fn(_) | Ty::Param(_)
        )
    }

    /// Bits of an integer width (unsigned types share the signed width).
    pub fn int_bits(self) -> u32 {
        match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 => 32,
            Ty::I64 | Ty::U64 | Ty::Usize => 64,
            _ => panic!("int_bits on non-int"),
        }
    }
}

pub const VEC_I64: Ty = Ty::Vec(Elem::I64);
pub const VEC_F64: Ty = Ty::Vec(Elem::F64);

/// A user-defined algebraic data type: variants with scalar (or other-ADT)
/// payload fields, in declaration order (the order is the runtime tag).
/// Generic ADTs are templates (`ty_params` non-empty, fields may name the
/// parameters); concrete instances are monomorphized copies with empty
/// `ty_params` (see `AdtInsts`).
#[derive(Debug, Clone, PartialEq)]
pub struct AdtDef {
    pub name: String,
    /// Type parameter names, in declaration order (indexed by the field
    /// `Ty::Param`s). Empty for concrete ADTs.
    pub ty_params: Vec<String>,
    pub variants: Vec<(String, Vec<Ty>)>,
}

/// Monomorphized ADT instances. Indices are `base + offset` where `base` is
/// the program's declared (`deftype`) ADT count; `build` appends `extra` to
/// the table after lowering, so every `Ty::Adt(i)` — declared or
/// instantiated — indexes the combined table everywhere downstream.
#[derive(Default)]
pub(crate) struct AdtInsts {
    /// (template index, type args) -> instance index. Dedupes instances.
    pub(crate) map: HashMap<(u32, Vec<Ty>), u32>,
    /// instance index -> (template index, type args). Lets substitution
    /// re-instantiate `(Option T)` once `T` is bound.
    pub(crate) rev: HashMap<u32, (u32, Vec<Ty>)>,
    pub(crate) extra: Vec<AdtDef>,
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
    /// `(rem a b)` — truncated remainder (the sign of `a`, like C's `%`).
    Rem,
    /// `(mod a b)` — floored modulo (the sign of `b`); integers only.
    Mod,
    /// `bit-and` / `bit-or` / `bit-xor` — integers only.
    BitAnd,
    BitOr,
    BitXor,
    /// `bit-shift-left` / `bit-shift-right` — integers only; the amount is
    /// taken modulo the width (never undefined), right shifts are
    /// arithmetic for signed and logical for unsigned widths.
    Shl,
    Shr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HExpr {
    ConstI64(i64),
    ConstF64(f64),
    /// `true` / `false`.
    ConstB(bool),
    Var(String),
    /// f32 arithmetic (f32 is a closed width like i32).
    BinF(BinOp, Box<HExpr>, Box<HExpr>),
    Count(Box<HExpr>),
    VecLit(VLit),
    /// A string literal; codegen mints a private `[N x i8]` constant.
    StrLit(String),
    /// `(conj v x)` — the persistent sequence with x appended. Vectors are
    /// values: `v` remains valid and unchanged. When the planner proves `v`
    /// is never read again it sets `in_place`, and codegen appends into v's
    /// buffer instead of copying — an unobservable optimization, not a
    /// semantic consume.
    Conj {
        v: Box<HExpr>,
        x: Box<HExpr>,
        elem: Elem,
        in_place: bool,
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
    /// Comparisons over closed widths: the Ty selects the LLVM type,
    /// signedness (signed vs `ult` family) and float vs int comparison.
    /// The only comparison form: width and signedness live in the node.
    CmpW(CmpOp, Box<HExpr>, Box<HExpr>, Ty),
    /// Explicit cast. Both widths are carried: `from` selects signedness
    /// for int→float and float→int conversion, `to` the target — neither
    /// backend reconstructs semantics from runtime values.
    ICast(Box<HExpr>, Ty, Ty),
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
        /// `Some(instance)` — the two-argument form returns `(Option V)`
        /// (the instance index of the prelude's Option). `None` — the
        /// explicit-default form returns V directly.
        res: Option<Ty>,
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
    /// `(< s1 s2)` (and `>`/`<=`/`>=`) on strings: `strcmp` sign test.
    StrOrd(CmpOp, Box<HExpr>, Box<HExpr>),
    /// `(nth s i)` — the byte at index `i` as an i64; runtime-bounds-checked
    /// (0 out of range, branchless select in codegen).
    StrNth {
        s: Box<HExpr>,
        idx: Box<HExpr>,
        /// Option-instance result for the two-argument form (see MapGet).
        res: Option<Ty>,
    },
    /// `(char->str c)` — a fresh one-byte heap string of c's low byte.
    /// Must be bound with `let` (allocates, like `subs`).
    CharStr(Box<HExpr>),
    /// `(loop [x :- ty init ...] body)` — Copy-typed variables, re-bound by
    /// `recur`; the loop's value is the body's completing value.
    Loop {
        binds: Vec<(String, Ty, HExpr)>,
        body: Box<HExpr>,
        ty: Ty,
    },
    /// `(recur e ...)` — re-bind the innermost loop's variables and jump to
    /// its head. Validated tail-only at lowering. The second field is drop-
    /// planner output: the bindings this iteration still owns at the jump
    /// (loop-body `let`s, the old values of loop variables not handed to
    /// the next iteration), freed after the arguments are evaluated and
    /// before the loop variables are rebound. `recur` is a scope exit.
    Recur(Vec<HExpr>, Vec<String>),
    /// `(str n)` over a numeric/bool operand: the runtime's decimal text.
    ToStr(Box<HExpr>, Ty),
    /// `(parse-i64 s)` / `(parse-f64 s)` — `res` is the `(Option T)`
    /// instance index synthesized by the lowering.
    ParseNum {
        s: Box<HExpr>,
        int: bool,
        res: Ty,
    },
    /// `(slurp path)` — whole-file read; empty string on failure.
    Slurp(Box<HExpr>),
    /// `(spit path content)` — writes (creates/truncates); yields 0.
    Spit {
        path: Box<HExpr>,
        data: Box<HExpr>,
    },
    /// `(args)` — process arguments as a `(Vector str)`; let-bound.
    Args,
    /// `(exit n)` — terminate the process with a status.
    Exit(Box<HExpr>),
    Reduce(Box<HReduce>),
    /// `(fn [x :- ty] body)` — a closure value capturing free locals by
    /// value. The body stays in the node (the interpreter evaluates it
    /// directly); the LLVM backend also gets a matching entry in
    /// `Program.lambdas` for out-of-line emission.
    Closure {
        sig: u32,
        /// Out-of-line body name (`@lambda_N`); the interpreter ignores it.
        mangled: String,
        params: Vec<(String, Ty)>,
        /// Free locals captured at creation, in env-slot order.
        captures: Vec<(String, Ty)>,
        /// Per-capture heap ownership (same order): a capture whose binding
        /// held static data (a literal) or an alias is NOT owned by the env,
        /// so the env's drop glue must not free it. Written by the drop
        /// planner from the binding's ownership state.
        capture_owns: Vec<bool>,
        body: Box<HExpr>,
    },
    /// `(f args...)` where `f` is a closure value.
    ClosureCall {
        f: Box<HExpr>,
        args: Vec<HExpr>,
        sig: u32,
    },
    Call(String, Vec<HExpr>),
    Let(
        Vec<(String, HExpr, Ty)>,
        Box<HExpr>,
        /// Bindings to free at this scope's exit (Phase 13): heap-owning
        /// bindings (Vec/Map/Set) that are provably unique here — never
        /// consumed by a call or conj, and never escaped by return.
        Vec<String>,
        /// Shadowed buffers to free while binding: `(name, index)` frees the
        /// storage `name` held *before* bind `index` overwrites it. A name
        /// rebound in one `let` (`[a x a (conj a y) ...]`) leaves each
        /// intermediate buffer unreachable once the next bind replaces the
        /// slot; the planner records those here so codegen releases them at
        /// the rebind instead of losing them.
        Vec<(String, usize)>,
    ),
    Seq(Vec<HExpr>),
    If(Box<HExpr>, Box<HExpr>, Box<HExpr>, Ty),
    And(Box<HExpr>, Box<HExpr>),
    Or(Box<HExpr>, Box<HExpr>),
    Not(Box<HExpr>),
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
        /// Option-instance result for the two-argument form (see MapGet).
        res: Option<Ty>,
    },
}

/// Typed vector literal contents (element type lives in `Ty::Vec`).
#[derive(Debug, Clone, PartialEq)]
pub enum VLit {
    I64(Vec<i64>),
    F64(Vec<f64>),
    /// A narrow integer vector literal (values range-checked at lowering).
    N(Narrow, Vec<i64>),
    F32(Vec<f64>),
}

impl VLit {
    pub fn elem(&self) -> Elem {
        match self {
            VLit::I64(_) => Elem::I64,
            VLit::F64(_) => Elem::F64,
            VLit::N(n, _) => Elem::N(*n),
            VLit::F32(_) => Elem::N(Narrow::F32),
        }
    }
    pub fn len(&self) -> usize {
        match self {
            VLit::I64(v) | VLit::N(_, v) => v.len(),
            VLit::F64(v) | VLit::F32(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HArm {
    /// Variant tag; `None` is the `_` wildcard arm.
    pub variant: Option<u32>,
    pub binders: Vec<String>,
    pub body: HExpr,
    /// Drop-planner output. `Some(skip)`: the scrutinee's heap box is freed
    /// when this arm completes, with the field slots of the binders in
    /// `skip` nulled first (their objects moved out, or are the arm's own
    /// value, so their duty lives elsewhere). `None`: no free here — the box
    /// stays with its owner's scope, or it is borrowed/aliased/loop-ridden.
    pub exit_free: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HReduce {
    /// `acc_ty` (both variants): the accumulator's type, `I64` or `F64`,
    /// decided once at lowering from the init's real type. Consumers read
    /// it — never re-derive it from the shape of `init`.
    Pipeline {
        combine: BinOp,
        init: HExpr,
        acc_ty: Ty,
        maps: Vec<MapSpec>,
        filters: Vec<FilterSpec>,
        src: HSource,
        /// Parallel-chunked emission (`(parallel (reduce ...))`, Phase 18).
        parallel: bool,
    },
    Closure {
        init: HExpr,
        acc_ty: Ty,
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
    /// Returned value is a borrow (`&T`): non-owning, non-droppable,
    /// non-movable at the call site.
    pub ret_borrowed: bool,
    /// Extern only: the C function hands its string/collection result over
    /// (`:- (own T)`, a `malloc`ed value the caller frees). Without it an
    /// extern result is borrowed: Cato never frees foreign memory.
    pub ret_owned: bool,
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
    /// Returned value is a borrow (`&T`); see `Defn::ret_borrowed`.
    pub(crate) ret_borrowed: bool,
    /// Extern result ownership (see `Defn::ret_owned`).
    pub(crate) ret_owned: bool,
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
    /// Monomorphized ADT instances (S1a).
    pub(crate) adt_insts: RefCell<AdtInsts>,
    /// Generic-function (multimethod) state: method table, hidden defns.
    pub(crate) gens: RefCell<GenState>,
    /// Closure signature table `(param tys, ret ty)`, deduped structurally;
    /// `Ty::Fn(i)` indexes it. Also drained into `Program::fn_sigs`.
    pub(crate) fn_sigs: RefCell<Vec<(Vec<Ty>, Ty)>>,
    /// Out-of-line closure bodies for the code backends.
    pub(crate) lambdas: RefCell<Vec<(String, Vec<(String, Ty)>, Vec<(String, Ty)>, Ty, HExpr)>>,
}

pub fn build(prog: &[Expr]) -> Result<Vec<Defn>, String> {
    Ok(build_program(prog)?.1)
}

/// Build a full program: the ADT table (from `deftype` forms) plus every
/// lowered `defn`.
mod build;
mod ctl;
mod forms;
mod generics;
mod infer;
mod lower;
mod match_lower;
mod mono;
mod numeric;
mod ownership;
mod parse;
mod pipeline;
mod stagefn;
mod typing;

pub use build::{build_program, build_program_located, build_program_located_with};
pub(crate) use ctl::*;
pub(crate) use forms::*;
pub(crate) use generics::*;
pub(crate) use lower::*;
pub(crate) use match_lower::*;
pub(crate) use mono::*;
pub use numeric::{eval_int_binop, Narrow};
pub(crate) use ownership::plan_drops;
pub use ownership::{compute_alias_ret, tail_may_alias};
pub(crate) use parse::*;
pub(crate) use pipeline::*;
pub(crate) use stagefn::*;
pub(crate) use typing::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Borrow {
    /// Takes the value (moves it at the call site).
    Owned,
    /// Immutable borrow: the caller keeps the value.
    Shared,
}

impl Borrow {
    pub fn is_owned(self) -> bool {
        self == Borrow::Owned
    }
}

/// Does this expression fork into basic blocks? (codegen-consistent)

impl<'a> Ctx<'a> {
    /// Resolve a constructor name to (adt index, variant index, field types).
    /// Searches the declared (template) table only — instantiated ADTs share
    /// their template's constructor names, and ctor calls instantiate on the
    /// fly (see `lower`).
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

    /// The ADT definition at combined-table index `i` (declared or
    /// instantiated). Returns a clone: instances live behind the RefCell.
    pub(crate) fn adt_def(&self, i: u32) -> AdtDef {
        let base = self.adts.len();
        if (i as usize) < base {
            return self.adts[i as usize].clone();
        }
        self.adt_insts.borrow().extra[i as usize - base].clone()
    }

    /// Is this combined-table index an instantiated (not declared) ADT?
    pub(crate) fn is_adt_inst(&self, i: u32) -> bool {
        (i as usize) >= self.adts.len()
    }

    pub(crate) fn instantiate_adt(&self, template: u32, args: &[Ty]) -> Result<u32, String> {
        instantiate_adt(self.adts, &self.adt_insts, template, args)
    }
}

/// Mangle one type argument for an instance name (defns use the same
/// convention; ADT arguments contribute their own mangled name).
fn adt_arg_suffix(t: Ty, base: &[AdtDef], insts: &RefCell<AdtInsts>) -> String {
    match ty_suffix(t) {
        Some(s) => s.to_string(),
        None => match t {
            Ty::Adt(i) => {
                let b = base.len();
                if (i as usize) < b {
                    base[i as usize].name.clone()
                } else {
                    insts.borrow().extra[i as usize - b].name.clone()
                }
            }
            // Template placeholders stay in the (index-stable) table but
            // never reach emitted code; the name just has to be LLVM-safe.
            Ty::Param(k) => format!("t{}", k),
            _ => "unk".to_string(),
        },
    }
}

/// Resolve an instantiation of a (possibly generic) ADT template for the
/// given concrete (or still-generic) type arguments. Deduped: the same
/// (template, args) pair returns the same combined-table index.
///
/// A template field naming another (or the same) template is applied to the
/// enclosing instantiation's arguments — the positional-parameter rule from
/// `parse_deftype_variants` — so recursive types instantiate through the
/// cache: the instance is registered *before* its fields resolve, and the
/// self-reference re-enters to a cache hit.
pub(crate) fn instantiate_adt(
    base: &[AdtDef],
    insts: &RefCell<AdtInsts>,
    template: u32,
    args: &[Ty],
) -> Result<u32, String> {
    let tpl = base
        .get(template as usize)
        .ok_or_else(|| format!("bad ADT index {}", template))?;
    if tpl.ty_params.len() != args.len() {
        return Err(format!(
            "{} expects {} type argument(s), got {}",
            tpl.name,
            tpl.ty_params.len(),
            args.len()
        ));
    }
    let key = (template, args.to_vec());
    if let Some(&i) = insts.borrow().map.get(&key) {
        return Ok(i);
    }
    let name = format!(
        "{}__{}",
        tpl.name,
        args.iter()
            .map(|t| adt_arg_suffix(*t, base, insts))
            .collect::<Vec<_>>()
            .join("_")
    );
    // Register the (empty) instance before resolving fields: recursion
    // through the template's own name re-enters the cache and terminates.
    let base_len = base.len();
    let idx = {
        let mut insts = insts.borrow_mut();
        let idx = base_len as u32 + insts.extra.len() as u32;
        insts.map.insert(key, idx);
        insts.rev.insert(idx, (template, args.to_vec()));
        insts.extra.push(AdtDef {
            name,
            ty_params: Vec::new(),
            variants: Vec::new(),
        });
        idx
    };
    let mut variants = Vec::new();
    for (vn, fields) in &tpl.variants {
        let fs: Vec<Ty> = fields
            .iter()
            .map(|f| subst_field_ty(*f, args, base, insts))
            .collect::<Result<_, _>>()?;
        variants.push((vn.clone(), fs));
    }
    insts.borrow_mut().extra[idx as usize - base_len].variants = variants;
    Ok(idx)
}

/// Substitute a template field with the instantiation's type arguments. A
/// field naming a generic template re-instantiates that template with the
/// same arguments (positional-parameter rule).
fn subst_field_ty(
    f: Ty,
    args: &[Ty],
    base: &[AdtDef],
    insts: &RefCell<AdtInsts>,
) -> Result<Ty, String> {
    let f = match f {
        Ty::Param(k) => args
            .get(k as usize)
            .copied()
            .ok_or_else(|| format!("unresolved type parameter {} in ADT field", k))?,
        Ty::Adt(j) => {
            let j_params = base.get(j as usize).map(|d| d.ty_params.len()).unwrap_or(0);
            if j_params > 0 {
                Ty::Adt(instantiate_adt(base, insts, j, args)?)
            } else {
                Ty::Adt(j)
            }
        }
        other => other,
    };
    Ok(f)
}

/// Is this ADT (combined-table index) recursive — on a cycle of
/// "contains an ADT-typed field" edges — and therefore heap-boxed?
/// Everything off the cycle stays inline/unboxed, including ADTs that
/// merely *contain* a boxed one (the boxed child is one pointer slot).
pub fn compute_boxed(adts: &[AdtDef]) -> Vec<bool> {
    compute_adt_facts(adts).boxed
}

/// Representation facts per combined-table ADT index.
/// - `boxed`: the ADT's value is a heap pointer. True for recursive types
///   (on an ADT-field cycle) AND for owning types — ownership requires a
///   unique address, so owning ADTs move by pointer like boxed ones.
/// - `owns`: the ADT owns heap memory (its box, or fields holding strings,
///   vectors, or owning ADTs) and needs drop glue.
pub struct AdtFacts {
    pub boxed: Vec<bool>,
    pub owns: Vec<bool>,
}

pub fn compute_adt_facts(adts: &[AdtDef]) -> AdtFacts {
    let n = adts.len();
    let mut boxed = cycle_boxed(adts);
    // owns = boxed ∨ direct-owning-data-field ∨ ADT-field(owns) — to fixpoint.
    let mut owns = boxed.clone();
    for _ in 0..=n {
        let mut changed = false;
        for (t, d) in adts.iter().enumerate() {
            if owns[t] {
                continue;
            }
            for (_, fields) in &d.variants {
                for f in fields {
                    let direct = matches!(f, Ty::Str | Ty::Vec(_) | Ty::Map | Ty::Set);
                    let via_adt =
                        matches!(f, Ty::Adt(j) if owns.get(*j as usize).copied().unwrap_or(false));
                    if direct || via_adt {
                        owns[t] = true;
                        changed = true;
                    }
                }
            }
        }
        // A newly-owning ADT must also be boxed (unique address for its
        // fields); and its box makes it owning — owns already covers that.
        for b in boxed.iter_mut().zip(owns.iter()) {
            if *b.1 && !*b.0 {
                *b.0 = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    AdtFacts { boxed, owns }
}

fn cycle_boxed(adts: &[AdtDef]) -> Vec<bool> {
    let n = adts.len();
    // Edges: t -> u for every ADT-typed field u.
    let mut edges: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (t, d) in adts.iter().enumerate() {
        for (_, fields) in &d.variants {
            for f in fields {
                if let Ty::Adt(u) = f {
                    if (*u as usize) < n {
                        edges[t].push(*u);
                    }
                }
            }
        }
    }
    // Iterative DFS with colors; a back edge boxes every node on the cycle
    // path from the target to the current node.
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;
    let mut color = vec![WHITE; n];
    let mut boxed = vec![false; n];
    for start in 0..n {
        if color[start] != WHITE {
            continue;
        }
        let mut path: Vec<usize> = Vec::new();
        let mut on_path = vec![false; n];
        // Explicit stack of (node, next-edge-index).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = GRAY;
        on_path[start] = true;
        path.push(start);
        while let Some(&mut (node, ref mut ei)) = stack.last_mut() {
            if *ei < edges[node].len() {
                let next = edges[node][*ei] as usize;
                *ei += 1;
                match color[next] {
                    WHITE => {
                        color[next] = GRAY;
                        on_path[next] = true;
                        path.push(next);
                        stack.push((next, 0));
                    }
                    GRAY => {
                        // Cycle: box everything from `next` up the path.
                        if let Some(pos) = path.iter().position(|&p| p == next) {
                            for &p in &path[pos..] {
                                boxed[p] = true;
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                color[node] = BLACK;
                on_path[node] = false;
                path.pop();
                stack.pop();
            }
        }
    }
    boxed
}

impl AdtDef {
    /// Payload slot count (i64 words) for each field of each variant.
    /// Scalars/bool/str/boxed-ADT fields take one slot, `(Vector _)` three,
    /// and an inline ADT child embeds its whole struct — tag plus payload —
    /// exactly the words its values materialize with (recursion terminates
    /// because inline children are cycle-free).
    pub fn field_slots(&self, boxed: &[bool], adts: &[AdtDef]) -> Vec<Vec<usize>> {
        let mut offsets = Vec::new();
        for (_, fields) in &self.variants {
            let mut per = Vec::new();
            let mut at = 0usize;
            for f in fields {
                per.push(at);
                at += field_slot_count(f, boxed, adts);
            }
            offsets.push(per);
        }
        offsets
    }
}

/// Payload slots a single field occupies.
pub fn field_slot_count(f: &Ty, boxed: &[bool], adts: &[AdtDef]) -> usize {
    match f {
        Ty::Vec(_) => 3,
        Ty::Adt(j) => {
            if boxed.get(*j as usize).copied().unwrap_or(false) {
                1
            } else {
                // Inline child: its tag plus its largest variant's fields —
                // the struct a value of that type materializes as. Without
                // the tag slot the child's by-value stores spill one word
                // past the parent box and every later field misreads.
                adts.get(*j as usize)
                    .map(|d| 1 + adt_payload_slots(d, boxed, adts))
                    .unwrap_or(1)
            }
        }
        _ => 1,
    }
}

fn adt_payload_slots(d: &AdtDef, boxed: &[bool], adts: &[AdtDef]) -> usize {
    d.variants
        .iter()
        .map(|(_, fields)| {
            fields
                .iter()
                .map(|f| field_slot_count(f, boxed, adts))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0)
}

/// Total payload slots for ADT `i` (the `[N x i64]` of its LLVM type).
pub fn adt_total_slots(adts: &[AdtDef], boxed: &[bool], i: u32) -> usize {
    adts.get(i as usize)
        .map(|d| adt_payload_slots(d, boxed, adts))
        .unwrap_or(0)
}
