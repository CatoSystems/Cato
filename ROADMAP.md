# Cato Roadmap

**Cato** is a statically-typed, compiled systems language with Clojure's surface
syntax: S-expressions, threading macros, immutable-first functional style —
compiling to native machine code with unboxed types, stream fusion, and
Rust-like ownership semantics. No GC, no boxing.

This roadmap expresses where the language is going and the order in which the
pieces land. Every phase follows the same method: a design spec in
`docs/superpowers/specs/`, task-by-task TDD commits that end green
(`cargo build` + `cargo test`), and an end-to-end reference program
(`.cato` → IR → `clang` → run).

## Design Principles

| Principle | How Cato Delivers It |
|---|---|
| **Clojure ergonomics** | S-expression syntax, literals, threading macros, Lisp macros |
| **Systems-level performance** | LLVM IR emission, monomorphization, no GC |
| **Memory safety without runtime cost** | Compile-time ownership/borrowing, zero runtime overhead |
| **Zero-allocation functional code** | Stream fusion fuses `map`/`filter`/`reduce` into a single allocation-free loop |
| **Honest slices** | Everything new is proven end-to-end (`.cato` → `.ll` → `clang` → run) before the next slice starts |

## Architecture

```
crates/
  cato-core/            backend-agnostic frontend: reader → expand → HIR
                        (typing, monomorphization, ownership check) → fusion
                        exposes Program { adts, defns } + the Backend trait
  cato-backend-llvm/    LLVM IR emitter (LlvmBackend): tagged-union ADTs,
                        fused single-loop pipelines, vectorization metadata
  cato-cli/             the `cato` binary: flags, --backend dispatch
runtime/runtime.c       C runtime helpers shared by every backend
```

New backends add a crate implementing `cato_core::Backend` plus a dispatch
line — see `docs/ARCHITECTURE.md`.

## Status — shipped and green

All reference programs in `examples/` run with exact expected output;
117 tests on `main`.

- **Frontend & fusion.** Reader; macros (`->`/`->>`, `when`, `when-not`,
  `cond`, `if-let`, `when-let`, `if-not`, `doseq`, variadic `and`/`or`);
  typed HIR; fusion check. Fused single-loop sources: vector variables,
  literals, `range`, `zip`, `take`/`drop`, `map`/`filter`. Reference:
  `slice.cato`, `normalize.cato`, `collections.cato`.
- **Types.** `i64`, `i32` (closed width, explicit casts), `f64`, `bool`,
  `str` (static constants, `strcmp`/`strlen`), `(Vector i64)` / `(Vector f64)`
  with generic `(Vector T)` monomorphization, user ADTs (`deftype` +
  exhaustive `match`, tagged unions), maps (`{k v}`, immutable snapshots,
  `get`/`assoc`/`count`). Reference: `shape.cato`, `generic.cato`,
  `widths.cato`, `strings.cato`, `maps.cato`.
- **Ownership.** Moves for non-Copy values, `&` shared / `&!` exclusive
  borrows, `clone`/`drop`, flow-sensitive use-after-move checking —
  compile-time only, zero runtime overhead. Reference: `own.cato`.
- **Heap.** Owned growable vectors (`{ptr, len, cap}`, in-place `conj` with
  amortized O(1) append), `subvec`, `for` comprehensions, `(str ...)`
  concatenation, vector-returning functions. Reference: `heap.cato`.
- **FFI & SIMD.** `(extern ...)` C declarations (direct calls, no
  marshaling); `--fast` attaches vectorization metadata to fused loops.
  Reference: `ffi.cato`.

## Next steps

### Phase 10 — Diagnostics with source spans *(next)*

Errors are strings today; make them precise and testable. Small phase, big
quality multiplier for everything after it.

- Reader produces byte spans; `Expr` (and then HIR nodes) carries them
  through macro expansion.
- HIR errors (`use of moved value x`, non-exhaustive match, width mixing,
  arity) report `file:line:col` with a caret snippet.
- `errors/` suite: negative programs with expected diagnostics asserted in
  tests; `--explain N` docs for the common codes.

### Phase 11 — Sets & string building

Completes the collection story on top of the Phase 7b heap.

- `#{1 2 3}` set literals; bit-packed integer sets for dense keys with a
  flat fallback; `conj`/`disj`/`contains?`/`count`.
- `(subs s start end)` CoW slices, `(split s sep)` / `(join sep xs)`
  producing heap vectors of strings.
- Reference: `examples/sets.cato`.

### Phase 12 — Remaining widths

The i32 slice established the `BinW`/`CmpW`/`ICast` pattern; the rest is
repetition plus one design decision per family.

- `i8`, `i16`, `f32`, `u8/u16/u32/u64/usize` — unsigned comparison opcodes
  (`icmp ult` family) and the signedness split are the real design work.
- Width-generic literals `(u8 255)` and literal inference rules.
- Reference: `examples/widths2.cato`.

### Phase 13 — Heap lifetimes: scope-exit frees & drop glue

Closes the interim "allocations live until process exit" contract.

- Last-use analysis in HIR (the move checker already tracks binding states —
  extend it to derive free points); frees at binding scope exit.
- Drop glue for ADTs holding vectors/strings/maps.
- `noalias` extended to user-owned pointers once uniqueness is proven.
- Unblocks allocation inside loops (today only scalar loop bodies are
  allowed) and in-place mutation for later phases.

### Phase 14 — First-class closures

Lambdas today exist only inline in `map`/`filter`/`reduce`. Generalize.

- Closures as values: capture environments and a calling convention, with
  the ownership interaction made explicit (captured-by-move vs by-borrow).
- `(fn ...)` usable anywhere an expression is; higher-order user functions.
- Fused pipelines keep working: statically resolvable stage functions stay
  defunctionalized; dynamic closures hit the calling-convention path.
- Reference: `examples/closures.cato`.

### Phase 15 — HAMT maps & transients

Upgrade the flat map slice to Clojure-grade persistent structures.

- Hash-array-mapped trie with structural sharing; the flat ≤N-entry map
  remains the small-case representation (it is the shipping behavior today).
- `transient`/`persistent!`-style batch mutation, safe under ownership;
  `into`; an iteration protocol shared with vectors.
- Bit-packed sets migrate onto the same trie for sparse keys.

### Phase 16 — Multi-backend: the interpreter

Prove the `Backend` trait with a second implementation and gain a tool.

- Tree-walking interpreter backend (`cato-backend-interp`) implementing
  `Backend::emit` as direct execution — same `Program`, same test suite.
- Doubles as a test oracle: differential testing interpreter-vs-LLVM across
  all of `examples/`, plus a `cato run` script mode.
- No LLVM dependency: fast, portable, and much better debugging.

### Phase 17 — Cranelift backend

A second native-code backend without the LLVM toolchain dependency.

- `cato-backend-cranelift` lowering HIR through Cranelift IR; faster
  compiles and smaller binaries. LLVM remains the optimizing default.
- Backend parity suite: one table of programs × backends × expected output.

### Phase 18 — Parallel loops

The systems endgame for the fusion story.

- `(parallel loop)` and auto-parallelization of provably disjoint fused
  loops (reduce requires an associativity check; map/filter/write are
  disjoint by index) over a small runtime thread pool.
- `--threads N`; loop-carried dependency rejection with clear diagnostics.
- Loops LLVM already vectorizes get chunked across cores.

### Phase 19 — Language & tooling base

The ergonomic floor for real programs.

- Modules/namespaces (`(ns ...)`, `require`), multi-file compilation.
- A REPL (cheap once the interpreter backend exists) and a basic LSP.
- Pattern-matching upgrades: destructuring in `let`/`for`/`match`, guards,
  `_` inside binder positions.

## Working method

- Each phase: design spec in `docs/superpowers/specs/`, TDD'd commits ending
  green (`cargo build` + `cargo test`), and an end-to-end reference program.
- Backward compatibility: reference programs from all earlier phases keep
  their exact output (e.g. `slice.cato` always prints `2.5` then `20`).
- Every loop keeps `noalias`/`inbounds`/`align 8` emission; numbers stay
  unboxed; nothing regresses to heap-allocated intermediates.
- Backend work requires parity: interpreter/Cranelift/LLVM must agree on the
  whole `examples/` suite.
