# Cato Roadmap

**Cato** is a statically-typed, compiled systems language with Clojure's surface
syntax: S-expressions, threading macros, immutable-first functional style —
compiling to native machine code via direct LLVM IR emission, with unboxed
types, stream fusion, and (eventually) Rust-like ownership semantics. No GC, no
boxing, no runtime.

This roadmap expresses where the language is going and the order in which the
pieces land. Every phase follows the same method: an architectural design
spec in `docs/superpowers/specs/`, a task-by-task implementation plan in
`docs/superpowers/plans/`, and a vertical slice that ships with tests plus a
compiled-and-run reference program.

## Design Principles

| Principle | How Cato Delivers It |
|---|---|
| **Clojure ergonomics** | S-expression syntax, literals, threading macros (`->`/`->>`), Lisp macros |
| **Systems-level performance** | Direct LLVM IR emission, monomorphization, no GC |
| **Memory safety without runtime cost** | Compile-time ownership/borrowing, zero runtime overhead |
| **Zero-allocation functional code** | Stream fusion fuses `map`/`filter`/`reduce` into a single allocation-free loop |
| **Honest slices** | Everything new is proven end-to-end (`.cato` → `.ll` → `clang` → run) before the next slice starts |

## Compiler Pipeline

```
.cato source
  → Reader            src/reader.rs   S-expressions → AST
  → Macro expansion   src/expand.rs   -> / ->> (and later: when, if-let, for, ...)
  → HIR build         src/hir.rs      fully concrete, typed AST
  → Stream fusion     src/fuse.rs     validates the single-loop fusion invariant
  → LLVM emission     src/codegen.rs  .ll with noalias / inbounds / align 8
  → clang             native binary
```

The vector-data pointer in every fused loop is `noalias nocapture readonly`,
GEPs are `inbounds`, loads are `align 8` — this is what lets LLVM vectorize and
auto-parallelize the language's pure loops.

## Status

Shipped and green on `main` (`cargo test`):

- **Slice 1 — Minimal vertical slice.** Reader, HIR builder, fusion check, LLVM
  emitter; typed `defn`, `let`, arithmetic over i64/f64, `(vec-i64 ...)`,
  `count`, fused `map`/`filter`/`reduce` emitting a single loop, `print-*`,
  CLI + runtime. Reference program: `examples/slice.cato`.
- **Slice 2 — Control flow, comparisons & threading macros.**
  `bool` type; `= /= < > <= >=`; `if`/`and`/`or`/`not` as phi-exact merges;
  `abs`/`min`/`max`; `->`/`->>` macro-expansion stage; typed pipeline stages
  (f64 `sitofp` map stages, i1 filter predicates); loop captures typed from
  function params; `even?` produces `i1`.
  Reference programs: `examples/normalize.cato` → `0.75`, `count-even-below` → `2`.
- **Slice 3 — Algebraic data types: `deftype` + `match`.**
  `(deftype Shape (Circle [radius :- f64]) ...)`; variant constructors
  resolved at call sites; `match` with exhaustive coverage checking and `_`
  wildcard; tagged-union LLVM representation `{ i64, [N x i64] }` with
  `bitcast`-based `f64` fields and entry-hoisted (loop-safe) constructor
  allocas; arm results merge via phi — scalars by value, ADT results as ptr
  phi (option→option transforms); ADTs cross function boundaries by value
  (no dangling stack pointers, no boxing).
  Reference program: `examples/shape.cato` → `19.635`, `12`, `16`, `4`.
- **Slice 4 — Collection API (fused core) + macros beyond threading.**
  `get` (bounds-checked, branchless); `range`, `take`, `drop`, `zip` and
  inline vector literals as **fused loop sources** — take/drop compile to
  branchless start/count arithmetic, zip binds two element variables, all in
  the same single loop with zero allocation. `when`/`when-not`/`cond`/
  `if-let`/`when-let`/`if-not`/`doseq` and variadic `and`/`or` desugar in
  `expand.rs` with no codegen changes. `conj`/`subvec`/`for` are deferred
  until vector materialization (Phase 7 heap story).
  Reference program: `examples/collections.cato` → `35 9 140 64 42 3 5 10 30`.
- **Slice 6 — Monomorphization.**
  `(Vector f64)` element-typed vectors in annotations, literals and fused
  loops (double GEP/loads, still `inbounds` + `align 8`); generic `defn`s
  with type parameters (`(defn gsum [xs :- (Vector T)] :- T ...)`) unified
  against call-site types and instantiated once per concrete type
  (`gsum__i64`, `gsum__f64`), fully unboxed. Generic bodies follow the `if`
  promotion rule for f64 elements.
  Reference program: `examples/generic.cato` → `10 2.5 7.25 9 2.5 50`.
- **Slice 7 — Ownership & borrows (enforcement layer).**
  Non-Copy values (ADTs, vectors) move on by-value use; `&` shared and `&!`
  exclusive borrows on params; `clone` and `drop`; flow-sensitive
  use-after-move checking in the HIR pipeline with branch-merge soundness.
  Compile-time only — the ABI is unchanged, so borrows cost nothing.
  Reference program: `examples/own.cato` → `123 3 7`.

Current type system: `i64`, `f64`, `bool`, vectors (`(Vector i64)` /
`(Vector f64)`, type-parameterized), user ADTs (scalar payload fields).

## Roadmap

### Phase 7b — Ownership: heap & escapes *(next)*

The remaining half of Phase 7, enabled by the enforcement layer: heap
allocation for escaping values, `noalias` extended to user pointers, runtime
drop glue — unlocking `conj`/`subvec`/`for` (deferred from Phase 4).

### Phase 8 — Maps, sets & strings

High-value data types for real programs.

- Flat-map HAMT (with a flat contiguous struct for ≤8 entries), bit-packed
  integer sets, copy-on-write strings.
- Literal syntax for `{}` / `#{}` / `"..."`.

### Phase 9 — FFI, width types & the runtime

The systems-programming endgame.

- `(ffi extern "name" ...)` declarations — direct C calls, no marshaling.
- `i8`/`i16`/`i32`/`f32`/`u*` widths and `usize`.
- Auto-SIMD and auto-parallelization of pure fused loops (`(parallel loop)`,
  `--fast`).

## Working Method

- Each phase is a design spec, an implementation plan, and TDD'd commits that
  end green (`cargo build` + `cargo test`) with an end-to-end reference program.
- Backward compatibility: reference programs from earlier phases keep their
  exact output (e.g. `slice.cato` must always print `2.5` then `20`).
- Every loop keeps `noalias`/`inbounds`/`align 8` emission; numbers stay
  unboxed; nothing regresses to heap-allocated intermediates.