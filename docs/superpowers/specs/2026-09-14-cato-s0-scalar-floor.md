# S0 — Scalar floor (self-hosting Phase S0)

Date: 2026-09-14
Goal: land the small scalar/string features the self-hosted kernel's reader and
emitter lean on, before the big S1 (generic/recursive ADTs) slice.

Four features, each proven end-to-end (`.cato` → IR → clang → run, plus
interpreter parity) and TDD'd. Reference program: `examples/strings2.cato`.

## 1. Boolean literals `true` / `false`

Today bools arise only from comparison/logic forms; there is no literal. The
reader parses `true`/`false` as symbols, which then type as `Ty::I64` via the
`Var` fallback — a silent wrong-type trap.

- AST: new `Expr::LitBool(bool)`; the reader's `parse_atom` recognizes the two
  tokens (before the symbol fallback). They are reserved: `true`/`false` can no
  longer be used as binding names.
- HIR: new leaf `HExpr::ConstB(bool)`, `ty_of` → `Ty::Bool`.
- LLVM: `i1` literal in bool position; zext'd 0/1 in i64 position (same as
  the existing bool-valued nodes).
- Interp: `Value::Bool`.
- Printing works through the existing `(print x :- bool)` prelude method
  (`1`/`0`), matching comparison results — no new printing rule.

## 2. String ordering: `<` `<=` `>` `>=` on `str`

`(= s1 s2)` compares content via `strcmp`; the ordering operators reject
strings ("strings support only = and /="). Ordering becomes the same runtime
comparison with the sign tested.

- Lowering (`forms.rs:lower_cmp`): the str/str branch accepts all six
  operators. `=`/`/=` keep `HExpr::StrCmp`; the ordering ops lower to new
  `HExpr::StrOrd(CmpOp, a, b)` (`Lt/Gt/Le/Ge` only — the others never reach
  it).
- Semantics: byte-wise lexicographic order, exactly `strcmp`'s sign test:
  `(str< a b)` ≡ `strcmp(a,b) < 0`. Mixed str/scalar stays a compile error.
- LLVM: `declare i32 @strcmp` (already declared for `StrCmp`), then
  `icmp slt/sgt/sle/sge i32 %c, 0`.
- Interp: Rust `str::cmp` (same ordering on byte strings).
- Fold: two string literals compare at compile time (mirrors `StrCmp` folding).

## 3. `(nth s i)` and `(char->str c)`

The reader needs byte access; the emitter needs byte construction.

- `(nth s i)` → `i64`: byte value at index `i`. Runtime-bounds-checked
  (0..strlen), branchlessly via `select` — out of range yields `0`, the same
  out-of-range contract as `(get vec idx)` with no default. Negative indexes
  are out of range. New `HExpr::StrNth { s, idx }`.
- `(char->str c)` → `str`: a fresh one-byte heap string (NUL-terminated) of
  the low byte of `c` (i8 truncation semantics, mirroring `(i8 c)`). Result is
  an owned heap value in the `subs` mold: must be bound with `let` (the
  "heap value must be bound with let" rule), lives until process exit per the
  interim no-free-for-str contract (heap frees cover `Vec|Map|Set` only).
  New `HExpr::CharStr(Box<HExpr>)`.
- LLVM: `StrNth` = `strlen` + `icmp ult` + `select` (clamped index) + byte
  `load` + zext + `select` to 0. `CharStr` = `malloc(2)` (already declared),
  store the truncated byte, store the NUL. Interp: byte indexing /
  `char::from_u32`-style construction on the truncated byte.
- Both backends must agree byte-for-byte, including OOB and truncation cases
  (covered in tests, not just the happy path).

## 4. Reader string escapes: `\t` and `\\` (sequential scan)

The current unescape is `raw.replace("\\n", "\n").replace("\\\"", "\"")` — a
second pass over already-substituted text, so `\\n` (backslash + n) becomes a
newline, and there is no tab escape. Replace with a single left-to-right scan
over the raw text:

- `\n` → newline, `\t` → tab, `\\` → backslash, `\"` → quote;
- any other `\x` keeps both characters verbatim (current lenient behavior,
  preserved so existing sources are unaffected).

## Non-goals

No char type (bytes are `i64`), no `parse-i64` (lands with `(Option a)` in
S1/S3), no `print-str` without newline (S3), no new diagnostics codes (errors
keep the existing plain-string shape).

## Test plan

- `reader_test.rs`: `true`/`false` literals, `\t`/`\\`/`\"`/`\n` escapes,
  unknown escape preserved, `\\n` ≠ `"\n"`.
- `hir_test.rs`: lowering/typing of the new forms; `true` types as Bool;
  ordering errors stay errors (mixed str/i64).
- Interp unit: `nth` OOB → 0, `char->str` truncation, ordering results.
- Parity: `examples/strings2.cato` added to the interp↔LLVM suite; CLI
  integration test asserting its exact expected output, ASAN-clean with
  `--asan`.
