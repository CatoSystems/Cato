# Cato Roadmap — Self-Hosting

**Cato** is a statically-typed, compiled systems language with Clojure's surface
syntax: S-expressions, threading macros, immutable-first functional style with
persistent collections — compiling to native machine code with unboxed types,
stream fusion, and compiler-derived ownership with deterministic destruction.
No GC, no boxing.

This roadmap has one goal: **Cato compiles Cato.** The Rust workspace becomes
the *seed* — a trusted stage-0 compiler — and a kernel written in Cato takes
over development of the language. Every phase follows the repo method: a design
spec in `docs/superpowers/specs/`, task-by-task TDD commits ending green
(`cargo build` + `cargo test`), and an end-to-end reference program with
interpreter↔LLVM parity.

## Bootstrap model

```
seed (Rust crates/)          trusted stage-0 compiler
  │  cato build selfhost/cato.cato
  ▼
cato1 (native, built by seed)
  │  cato1 build selfhost/cato.cato
  ▼
cato2 (native, built by cato1)
```

The acceptance test for the entire roadmap: **`cato1 --emit-ir` and
`cato2 --emit-ir` over `selfhost/cato.cato` produce byte-identical output**,
asserted in CI on every push. Until then the seed is trusted for everything;
afterwards the seed is frozen at a tag and exists only to rebuild stage 1 from
source (the same trust position as `rustc`'s stage0). Language changes then
land in `selfhost/` and ship by re-bootstrapping.

## The subset contract

The kernel implements exactly the language its own source is written in — not
the seed's full surface. `selfhost/` source therefore avoids, by convention:

- `&` borrows on parameters (the only borrow form; closures and collections
  are move-only — lend or clone) — the kernel may also do plain move checking
  only, per slice;
- `defgeneric`/`defmethod`, `reduce`/`for`/`doseq` and all fusion machinery,
  `parallel`, `--fast` vectorization — iteration is `loop`/`recur`;
- multi-file `require` until after the fixpoint (one namespace, then split);
- the seed's `print` multimethod prelude — the kernel ships its own minimal
  prelude of plain functions.

Anything outside the contract must fail with a clear
`cato1: unsupported ...` diagnostic, never silently miscompile.

The seed's own test suite keeps guarding everything it covers; the kernel adds
its own suites (differential against the seed where representations overlap).

## Baseline — what the seed ships today

- Frontend: reader (byte spans on top-level forms), fixed macro set, typed HIR
  (widths and signedness in the IR), CLOS-style multimethods, monomorphization,
  ownership check with ownership-driven destruction (moves transfer the drop
  duty), fusion check; middle end (fold, DCE/copy-prop, devirt, inline).
- Semantics: persistent collections (conj/assoc/disj never consume; in-place
  update only on provable last use), move-only closures with owned, freed
  environments, refcounted HAMT map nodes, two-argument `get`/`nth` returning
  `(Option V)`.
- Backends: LLVM IR text emission (tagged unions, fused loops, closures,
  collections) and the tree-walking interpreter, byte-identical on all of
  `examples/`; ~222 tests.
- Tooling: `cato build/run/repl/lsp`, wasm32 emission, coded diagnostics,
  `runtime/runtime.c` helpers linked by every native build.
- Zero external crates; everything above is the space the kernel must
  eventually cover or explicitly retire.

**Status (September 2026):** S0–S3 are shipped, plus the memory-model revision
(`docs/superpowers/specs/2026-09-18-cato-ownership-model-revised.md`): widths
and signedness live in the IR, destruction derives from ownership states,
collections are persistent with last-use in-place update, closure environments
are owned and freed, HAMT nodes are refcounted, `&!` is removed, and
two-argument `get`/`nth` return `Option`.
**S4 is shipped** (`selfhost/cato/lib/`, spec
`docs/superpowers/specs/2026-09-19-cato-s4-compiler-stdlib.md`): symbol
interning, string builder, span/AST ADTs, sid-keyed assoc-lists, and Result
plumbing — ordinary Cato built by the seed, ASAN-clean, guarded in the test
suite from this phase onward. **S5 is shipped** (`selfhost/cato/reader/`,
spec `docs/superpowers/specs/2026-09-19-cato-s5-kernel-reader.md`): the
kernel reader — tokenizer + iterative scan producing the span-carrying AST,
byte-matching the seed reader over every `examples/` file and an edge-case
fixture corpus, with interp↔LLVM parity of the driver itself. **S6 is
shipped** (`selfhost/cato/hir/`, spec
`docs/superpowers/specs/2026-09-19-cato-s6-kernel-expander-lowering-typing.md`):
the kernel expander (the fixed macro set), forms parsing, lowering +
monomorphization, move checking + drop planning, and the wire dump — the
full pipeline's typed-IR output is byte-identical to a Rust oracle over the
seed's `build_program_located` on the fixtures corpus, and the expander
differential covers every `examples/` file. S7–S9 are designed but
un-attempted. The phases below are the plan, not a record.

## Phase S0 — Scalar floor *(shipped)*

Small, pure language additions every later slice leans on. Shipped:
`true`/`false` literals, string ordering via the strcmp sign test,
`(nth s i)` / `(char->str c)`, sequential-scan string escapes (`\t`, `\\`),
and the reference program `examples/strings2.cato` with full interp↔LLVM
parity, ASAN-clean. Also fixed along the way: bool bindings emitted as raw
i1 registers in i64 position, and string-valued `if`/`let`/`seq` expressions
failing codegen.

- `true`/`false` literals (today bools arise only from comparison forms).
- String ordering: `<` `<=` `>` `>=` on `str` via `strcmp` semantics (the
  `StrCmp` plumbing in `cato-core/src/hir/forms.rs` already exists for `=`).
- `(nth s i)` → `i64` (byte value, bounds-checked) and `(char->str c)`;
  `\t` and `\\` escapes in the reader.
- Reference: `examples/strings2.cato`; interp↔LLVM parity.

## Phase S1 — Generic & recursive ADTs + drop glue *(shipped — see `docs/superpowers/specs/2026-09-15-cato-s1-adt-memory-semantics.md`)*

The biggest language slice; every compiler data structure depends on it.

- Type parameters on `deftype`: `(deftype (Option a) (Some [v :- a]) None)` —
  monomorphized with the existing `Ty::Param`/unify machinery used by generic
  `defn`s.
- ADT fields over any type — `str`, `(Vector T)`, other ADTs, recursion —
  lifting the scalar-only rule in `crates/cato-core/src/hir/parse.rs`
  (`parse_scalar_ty`).
- Drop glue: scope-exit frees for heap-owning ADTs, extended from
  `annotate_frees`; ASAN-verified; use-after-move through ADT fields.
- `match` over heap ADTs with move/borrow of fields; constructors return by
  value like vectors and maps already do.
- Reference: `examples/trees.cato` — a recursive tree built, folded, dropped,
  ASAN-clean.

## Phase S2 — `loop`/`recur` + rich closures *(shipped — `examples/loops.cato`)*

- `loop`/`recur` with general while-loop codegen — the LLVM backend today
  emits only fused `reduce` loops and `for` fill loops; `recur` targets the
  innermost `loop` or the enclosing fn head, compile error in non-tail
  position.
- Closures capture any value type by move (boxed env for non-scalar captures
  under the `%Closure {code, env}` convention).
- Reference: `examples/loops.cato`; parity; ASAN-clean.

## Phase S3 — Host surface: numbers↔strings, files, argv, exit, spawn *(shipped — `examples/host.cato`)*

The compiler is a program that reads files, writes files, and starts other
programs — none of which the language can do today.

- `(str n)` for every numeric width (number→string; the emitter cannot emit IR
  without it); `(parse-i64 s)` / `(parse-f64 s)` returning `(Option i64)` /
  `(Option f64)` (S1 pays off immediately).
- `slurp`/`spit` as runtime.c helpers shared by both backends; raw
  `print-str` without a trailing newline (all three current printers append
  `\n` in `runtime/runtime.c`).
- argv and exit codes: `(args)` → `(Vector str)` and `(exit n)`; the emitted
  `main` grows argc/argv plumbing (today it is `define i32 @main()` with a
  hardcoded `ret i32 0` in `cato-backend-llvm`).
- Spawning the C compiler via `(extern ...)` (`system`/`popen`). The
  interpreter must model every new builtin/extern natively (it currently knows
  only `fabs` and `strlen`) so parity stays meaningful and `cato run` works on
  compiler code.
- Reference: `examples/host.cato` — reads a file, writes a file, prints its
  args, exits nonzero.

## Phase S4 — Compiler stdlib in Cato (`selfhost/cato/lib/`) *(shipped — `docs/superpowers/specs/2026-09-19-cato-s4-compiler-stdlib.md`)*

Ordinary Cato, built by the seed, guarded by the parity suite.

- Symbol interning: `str` → `i64` sid with a `(Vector str)` table; sid-keyed
  `Map i64 V` tables serve as environments (today's maps are `i64`→`i64`).
- String builder: accumulate a `(Vector str)`, join once — emitter output and
  diagnostics.
- Span and AST ADTs (recursive — S1 exercised for real), small assoc-list and
  error-plumbing utilities.

## Phase S5 — Kernel reader

- Tokenizer + iterative (stack-based, no recursion) reader producing a
  span-carrying AST: `;` comments, strings, `#{...}`, `{...}`, `[...]`,
  `(...)`, i64/f64/symbol atoms — semantics exactly matching
  `crates/cato-core/src/reader.rs` (including "no keywords: `:when` is a
  symbol").
- Acceptance: differential test — kernel reader vs seed reader over every file
  in `examples/` (including `errors/`, `modules/`), same AST serialization.

## Phase S6 — Kernel expander, lowering, typing *(shipped — `docs/superpowers/specs/2026-09-19-cato-s6-kernel-expander-lowering-typing.md`)*

- The fixed macro set (`->` `->>` `when` `when-not` `cond` `if-let`
  `when-let` `if-not` `and` `or`) mirroring `cato-core/src/expand.rs`.
- `defn`/`extern`/`deftype` parsing; typed IR for the subset: `let` with
  destructuring, `if`, `match` (+ `:when` guards), closures, calls, width
  casts, arithmetic/comparisons, string ops, ADT ctors, `clone`/`drop`,
  `loop`/`recur`.
- Move checking (no borrows, per the contract).
- Monomorphization for generic `defn`s and ADTs using the seed's mangled-name
  scheme so emitted symbols match.
- Acceptance: kernel type/IR dumps agree with the seed's `--dump-hir` on
  subset programs. *(Met: the `hir` differential in
  `crates/cato-backend-interp/tests/kernel_hir.rs` byte-matches a Rust
  oracle over the seed's `build_program_located` — the pre-optimization
  stage — against the kernel's wire dump on
  `selfhost/cato/hir/fixtures/`; the expander differential additionally
  covers every `examples/` file in `ast` mode. Native codegen of the full
  kernel driver hits a seed emitter bug and is deferred to S7, so the
  driver itself runs through the seed interpreter.)*

## Phase S7 — Kernel LLVM emitter *(shipped)*

- Textual IR via the string builder: `%VecI64`/`%Closure`/tagged-union ADT
  layouts, `malloc`/`free` declares, runtime.c calls — a Cato port of the seed
  emitter's *shape* for the subset, not its full feature set.
- Acceptance: emit → `clang` → run over a growing program suite; where the
  subset overlaps the seed, byte-compare against seed `--emit-ir` output
  (informational signal, not a requirement). *(Met: the emit differential in
  `crates/cato-backend-interp/tests/kernel_emit.rs` drives the kernel driver
  (read → expand → build → moveck → emit) over the S6 fixtures, clangs the
  module with `runtime/runtime.c`, runs the binary, and byte-matches stdout
  against the seed interpreter — `host.cato`'s argv-derived stdout is exempt
  (S3 interp excludes argv[0]), and dump-mode fixtures without a `main` get
  a compile-only clang check. The seed `--emit-ir` byte-compare stays a
  development-time signal.)*

## Phase S8 — Stage 1: the kernel builds itself

- Kernel CLI skeleton: read source → compile → write `.ll` → spawn `cc` →
  native binary, with real exit codes.
- `cato build selfhost/cato.cato` (seed) → **cato1**; `cato1 build
  selfhost/cato.cato` → **cato2**; cato2-produced binaries run all reference
  programs and agree with `cato run` parity output.

## Phase S9 — Fixpoint, CI, seed freeze

- `diff <(cato1 --emit-ir selfhost/cato.cato) <(cato2 --emit-ir
  selfhost/cato.cato)` — byte-identical; CI job runs it on every push.
- Document the rebuild path (seed → cato1 → cato2) in `docs/`; tag the seed.
- From here: language changes land in `selfhost/`, are compiled by the current
  stage-1 binary, and ship by re-bootstrapping to a new fixpoint.

## After the fixpoint (not scheduled)

Split `selfhost/` into namespaces (kernel grows `require` merging); coded
diagnostics at seed quality; the REPL/LSP port-or-retire decision; performance
(interning is in from S4; arena AST only if clone-heavy passes show up in
profiles); then the fun parts — `quote`/syntax-quote and user-defined macros,
fusion applied to the kernel's own loops, and backends beyond LLVM.

## Working method

- Each phase: design spec in `docs/superpowers/specs/`, TDD'd commits ending
  green (`cargo build` + `cargo test`), and an end-to-end reference program.
- Backward compatibility: all existing `examples/` keep their exact output
  after every phase; interpreter and LLVM stay byte-identical.
- Kernel slices carry their own differential tests against the seed wherever a
  representation overlaps (reader output in S5, typed IR in S6, emitted text
  in S7) — the seed is the oracle until S9 makes the fixpoint the oracle.
- The seed's suite never regresses: `selfhost/` is built by the seed in CI
  from S4 onward, so kernel source is always compilable, from day one.
