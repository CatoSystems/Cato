# Cato Diagnostics Design (Phase 10)

## Goal

Replace bare error strings with precise, coded, testable diagnostics: every
error carries a stable code (`E301`), and — where a span is known — a
`file:line:col` location with a caret snippet, rustc-style.

## Design

### Spans

`ast::Span { lo, hi }` is a byte-offset range into the source text. The reader
tracks byte positions and produces:

- exact spans for reader errors (unbalanced delimiter, unterminated string,
  unclosed delimiter, bad map literal),
- a top-level form span for every form (`read_all_spanned` returns
  `(Vec<Expr>, Vec<Span>)`, parallel to the forms).

### Source registry

`cato_core::diagnostics` holds a thread-local `SourceMap` (file name, source
text, line starts) installed by `compile_named(file, src)` /
`compile_to_program(src)` (which uses `"input.cato"`). Direct calls to
`read_all` / `build_program` (existing tests) have no source registered;
their errors render without a location line.

### Attribution & rendering

Errors stay `String` through the pipeline (every existing `contains`
assertion keeps working). Location is attached at the point where the
responsible unit is known:

- reader errors — exact span,
- macro-expansion errors — the top-level form's span,
- lowering and ownership errors — the enclosing `defn`'s span (attached per
  defn in `build_program_located`).

`diagnostics::locate(span, msg)` renders:

```
[E301] use of moved value `b`
  --> examples/errors/use_moved.cato:5:1
   |
 5 | (defn bad [b :- Box] :- i64 (take2 b b))
   | ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
```

This slice attributes errors at defn granularity; per-HIR-node spans are a
follow-up refinement of the same mechanism.

### Codes

Codes are derived from the message by `diagnostics::classify` (stable
substring rules), so no error site needs editing:

| Code  | Meaning |
|-------|---------|
| E001  | unbalanced delimiter |
| E002  | unterminated string |
| E003  | map literal missing a value |
| E004  | unclosed delimiter at EOF |
| E101  | macro expansion shape error |
| E201  | arity error |
| E202  | invalid form / argument type |
| E203  | ambiguous generic-function dispatch |
| E204  | uncovered generic-function dispatch |
| E205  | unreachable / duplicate generic-function method |
| E206  | unbound variable |
| E207  | unknown function |
| E208  | unknown type name (lowercase unknowns are typos, not type parameters) |
| E301  | use of moved value |
| E303  | move of a borrowed parameter (the caller still owns it) |
| E401  | non-exhaustive match |
| E000  | uncategorized |

`cato --explain E301` prints the long-form documentation for a code.

### errors/ suite

`examples/errors/*.cato` are negative programs. A dedicated test compiles
each through `compile_named` and asserts the expected code, message
substring, and `file:line:col` — the suite is the regression net for the
diagnostic format.
