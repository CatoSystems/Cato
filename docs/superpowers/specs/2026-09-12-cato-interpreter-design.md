# Cato Interpreter Backend Design (Phase 16)

## Goal

Prove the `Backend` trait with a second implementation and gain a tool:
a tree-walking interpreter (`cato-backend-interp`) with no LLVM dependency,
used as a differential-testing oracle and as `cato run` script mode.

## Design

### Values

- Integers of every width are stored as `i64`. LLVM emits wrapping ops at
  each width, so the interpreter wraps at the width too (`wrap`): unsigned
  values are normalized into `0..2^bits` (the unsigned reading), signed
  values are sign-extended. Width-correct comparisons then reduce to plain
  i64 compares — mirroring how LLVM emits `icmp` over the width type.
- `Bin` carries no width in HIR, so the interpreter mirrors the lowering
  rule: float when either operand is float (the i64→f64 auto-promotion),
  wrapping ints otherwise. `BinW`/`BinF`/`CmpW` carry their width/float
  kind explicitly.
- Casts implement the same matrix as the LLVM `emit_cast`:
  trunc/sext/zext/sitofp/uitofp/fptosi/fptoui/fptrunc/fpext.
- Vectors are `Vec<Value>` snapshots; maps sorted `(i64,i64)` entries
  (assoc filter-copies, last-match-wins get); sets sorted unique entries.
  Strings are byte strings; `subs`/`split`/`join` implement the runtime
  helpers natively.

### Backend trait

`InterpBackend::emit` *executes* the program (`main`) and returns the
captured stdout as the emitted artifact. Builtins (`print-*`, `even?`) and
the modeled externs (`fabs`, `strlen`) are evaluated natively; unknown
externs are a runtime error.

### CLI

`cato run <file.cato>` (or `--backend interp`) runs the program directly
and prints its output — no IR, no clang.

### Parity

The differential suite (`cato-interp/tests/parity.rs`) runs every program
in `examples/` through both backends — interpreter directly, LLVM via
clang — and asserts identical stdout. This is the backend parity gate the
roadmap requires; new phases extend `examples/` and automatically extend
the parity suite.
