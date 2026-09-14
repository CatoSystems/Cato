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

All reference programs in `examples/` run with exact expected output on
both backends, with interpreter-vs-LLVM parity asserted in tests and
AddressSanitizer-clean heap behavior; ~140 tests on `main`.

- **Frontend & fusion.** Reader (byte spans); macros; typed HIR; fusion
  check; stream fusion sources: vector variables, literals, `range`,
  `zip`, `take`/`drop`, `map`/`filter`. Reference: `slice.cato`,
  `normalize.cato`, `collections.cato`.
- **Types.** `i64`, `i32`, `i8`, `i16`, `u8/u16/u32/u64/usize`, `f64`,
  `f32` (closed widths, explicit casts, unsigned `ult`-family
  comparisons), `bool`, `str`, `(Vector i64|f64|str)` with generic
  monomorphization, user ADTs + exhaustive `match` (wildcard binders,
  `:when` guards with fall-through), maps (flat + persistent HAMT with
  structural sharing), sets (`#{...}`, `conj`/`disj`/`contains?`),
  `(subs)`/`(split)`/`(join)`. References: `shape.cato`, `generic.cato`,
  `widths.cato`, `widths2.cato`, `strings.cato`, `maps.cato`,
  `hamt.cato`, `sets.cato`.
- **Ownership.** Moves, `&`/`&!` borrows, `clone`/`drop`, use-after-move
  checking, and scope-exit frees for non-escaping heap bindings
  (ASAN-verified). Reference: `own.cato`, `heap.cato`.
- **Closures.** First-class `(fn ...)` values: capture-by-value envs,
  `(Fn [t1 t2] ret)` higher-order params, out-of-line lambda bodies under
  the LLVM `%Closure {code, env}` convention; fused pipelines unchanged.
  Reference: `closures.cato`.
- **FFI & SIMD.** `(extern ...)` direct C calls; `--fast` vectorization
  metadata. Reference: `ffi.cato`.
- **Parallelism.** `(parallel (reduce + 0 xs))` — qualified reduces chunk
  across pthreads with identical output at any `--threads N`. Reference:
  `parallel.cato`.
- **Diagnostics.** Coded errors (`E001`–`E401`) with `file:line:col` +
  caret snippets; `errors/` suite; `cato --explain CODE`.
- **Multi-backend.** `Backend` trait proven twice: LLVM and a
  tree-walking interpreter with `cato run` and a differential parity
  suite over all of `examples/`.
- **Tooling.** `(ns ...)` + `(require [path :as alias])` multi-file
  compilation (`examples/modules/`), `cato repl` (Clojure-style: namespace
  prompt, multi-line forms with `#_=>` continuation, editable lines with
  persistent history via a dependency-free stty raw-mode editor, colored
  results/errors), `cato lsp` (diagnostics, definition, hover).
- **Native builds from the CLI.** `cato build` links the emitted IR with
  `runtime/runtime.c` into a native binary (system C compiler, `-O`
  levels, `--emit-ll`, `--asan`); `cato run --native` builds and executes
  in one step with exit-code propagation; `cato run` remains the fast
  interpreter mode. All modes agree byte-for-byte on program output.
- **Targets & opt-in emission.** IR is emitted only when asked for
  (`--emit-ir`); bare `cato <file>` is a compile check with a one-line
  stderr summary. The `--target wasm32` option emits
  `wasm32-unknown-unknown` modules — the IR is asserted to pass LLVM's
  wasm codegen — with `cato build --target wasm32` linking `.wasm` where a
  wasm-capable clang + lld is available.
- **CLI ergonomics.** `cato help` / `--help` on stdout with documented
  exit codes (0 ok, 1 compile/runtime error, 2 usage); a friendly no-args
  banner; did-you-mean suggestions for mistyped flags and commands; TTY
  colors with `NO_COLOR` support; check/build success lines with timing.
- **Middle end.** HIR optimization pipeline in `cato-core/src/opt/`:
  constant folding (wrapping, backend-exact semantics), DCE +
  copy-propagation, multimethod devirtualization, inlining, and clone
  elision with free-list transfer — fixed-point driver with a free-list
  recompute + ownership re-verify safety net. On by default (`--no-opt`
  skips); both backends benefit; guarded by the parity suite and an
  opt-off/opt-on interpreter differential.

## Next steps

### Generics, Common Lisp style — shipped (slice 1-2, 4)

`defgeneric`/`defmethod` multimethods over closed ADTs, resolved through
a structural specificity partial order (variant > whole ADT,
component-wise for multiple dispatch) into compiler-generated dispatch
functions holding nested matches — no runtime method tables, no
declaration-order semantics. Ambiguous (incomparable) and uncovered
combinations are compile-time errors with dedicated diagnostics (E203/
E204/E205). `print` is a builtin multimethod; all examples use it.
`call-next-method` and method combination remain deferred — see
`docs/superpowers/specs/2026-09-13-cato-clos-generics-design.md`.

The dispatch domain now includes builtin collection types: `defmethod`
specializers accept concrete vectors — `(Vector i64)`, `(Vector f64)`,
`(Vector str)` — plus `map` and `set`, alongside ADTs, scalars and
variants, with exact resolution (a call over an uncovered type is a
compile-time E204). Vector specializers also accept the wildcard
`(Vector _)`: it matches any element type and the method body is
monomorphized per call site into `name__m{index}__{elem}` instances
(exact specializers are strictly more specific and win where both
apply). Underneath it, the LLVM backend gained the collection calling
convention: maps pass as a `(ptr, len, cap)` triple, sets as a
`(ptr, len)` pair, each copied into a callee-local header slot so owned
in-place growth rewrites the callee's slot, never the caller's frame —
and maps and sets also **return** by value in the same header shape,
composing with `get`, `count` and dispatch at the call site.

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
