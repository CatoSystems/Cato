# S5 — Kernel reader (`selfhost/cato/reader/`)

Date: 2026-09-19
Goal: the kernel reads Cato source. A tokenizer + tree assembler written in
Cato, built by the seed, produces a span-carrying AST with semantics exactly
matching `crates/cato-core/src/reader.rs` — including `;` comments, strings
with the lenient escape pass, `#{...}`/`{...}`/`[...]`/`(...)`, i64/f64/bool/
symbol atoms, and "no keywords: `:when` is a symbol". Acceptance: a
differential test runs kernel reader vs seed reader over every file in
`examples/` (including `errors/`, `modules/`) plus a fixture corpus, and the
AST serializations agree byte for byte.

## Surface contract (mirrored from the seed reader, byte for byte)

- Whitespace: space, tab, newline, CR. A newline bumps the line counter and
  resets the column; every other byte bumps the column.
- `;` comments run to end of line (or EOF).
- Delimiters: `(`, `[`, `{`, `#{`; closers `)`, `]`, `}`, `}`. A `}` closes a
  map or a set; nothing else closes across kinds. Errors carry the exact
  span of the offending byte:
  - `unbalanced delimiter '<c>'` — wrong closer, or a closer with an empty
    stack (span: the closer byte, length 1);
  - `map literal must have key/value pairs` — odd child count (span: the
    whole `{...}` form);
  - `unterminated string` (span: opening quote through EOF);
  - `unclosed delimiter` at EOF (span: the *bottom* (first-opened) delimiter
    byte, length 1).
- Strings: scanning skips a `\`-escaped byte (so `\"` does not end the
  string); the unescape pass is single left-to-right — `\n` `\t` `\\` `\"`
  translate, any other `\x` keeps both characters, a trailing lone `\` is
  kept. The AST stores the unescaped value; the span includes both quotes.
- Atoms run until whitespace, a delimiter, `;`, or `"`. Classification order
  (matching `parse_atom`): i64 via `parse-i64` (Rust `from_str` grammar: `+5`
  and `-5` are ints; overflow is a symbol); `true`/`false`; if the token
  contains `.` and `parse-f64` accepts it, f64; otherwise a symbol.
- The seed's reader is a fused single pass over the bytes with explicit
  stacks — the kernel is the same state machine. "Tokenizer + reader" in the
  ROADMAP names the two concerns (lexical dispatch, tree assembly), which
  share the explicit stack; a separate token ADT would double the clone
  traffic with no semantic gain.

## Spans

The seed AST carries byte offsets only on top-level forms (`read_all_spanned`
returns `(Vec<Expr>, Vec<Span>)`); inner nodes are bare. The kernel AST (S4's
`Ast`) carries a `Span` on every node, so the kernel scan tracks
`(line, col)` as it consumes bytes: line starts at 1, col at 0, newline
resets col, every byte consumed advances col (bytes, not characters). A
node's span is the opening byte's `(line, col)` with `len = hi - lo` — for
collections, including both delimiters; for strings, both quotes.

The differential contract below pins the *top-level* spans (the only ones
the seed produces). Inner spans ride on the same counters and are exercised
by the reference driver printing a hand-checked sample.

## S4 amendment: `ABool`

The seed reader produces `LitBool` for `true`/`false`, and S4's `Ast` has no
bool variant. `ast.cato` gains `(ABool [v :- i64 at :- Span])` (tag 9, 0/1)
with the discriminant/count/child-count matches updated. This is the one S4
change S5 makes; the S4 reference output is unaffected (it never builds
`ABool`).

## Kernel-side shape (`selfhost/cato/reader/`)

One namespace concatenated after `selfhost/cato/lib/` (canonical order below;
`reader_ref.cato` defines the only `main`).

### `read.cato` — the scan

- `(deftype RStack (RSNil) (RSFrame [kind :- i64 open :- i64 oline :- i64
  ocol :- i64 kids :- Asts next :- RStack]))` — the explicit parse stack.
  `kind`: 0 list, 1 vector, 2 map, 3 set. `kids` accumulates by persistent
  prepend (reversed; close reverses once).
- `(deftype RRes (ROk [forms :- Asts n :- i64 tab :- (Vector str)]) (RErr
  [msg :- str at :- Span]))` — top-level forms in source order with the
  symbol table (sids are table indices; the serializer renders names), or a
  reader error with its span.
- `(read-all src)` → `RRes`: entry point, seeds the scan at byte 0, line 1,
  col 0.
- `scan` carries the heap state as *parameters* — `src i line col stack top
  tab` — and threads it by move: plain steps (whitespace, comments, atoms,
  delimiter open) rebuild nothing and clone nothing. The S4 extraction law
  (match-bound fields alias the box; extraction clones) is paid only where
  the state actually changes:
  - delimiter open: `(RSFrame ... stack)` boxes the parameter — free;
  - top-level push: `(KCons node top)` on a parameter — free;
  - delimiter close: the frame is matched, so `kids` and the remaining
    stack are cloned out (deep copies, freed with the originals when the
    consumed box drops) — O(closing collection + enclosing) per close;
  - interning: the table is a `(Vector str)` parameter; `intern-find` reads
    it through `get` (shared borrow, no clone), a miss does `(conj tab tok)`
    — a provable last use grows in place, no copy. Net: zero clone traffic
    for symbol lookup, one in-place append per distinct symbol.
- Helpers are the seed's helpers, ported: `skip-line`, `str-end` (backslash
  skip), `tok-end` (delimiter/whitespace stop set), `unescape` (run-copying:
  copy the bytes up to the next `\` with `subs`, translate the escape,
  recurse — no per-byte string building), `asts-rev`, and the byte predicates
  (`is-ws`, `is-close`, `kind-accepts`).
- Iteration shape: the scan is a tail-recursive state machine — one lexical
  event per self-call — not recursive descent. There is no TCO in the seed
  backend, so scan depth is O(total tokens) and each helper (comment skip,
  string scan, token scan) adds O(its length) transiently. At examples
  scale (≤ a few KB) this is nothing; S8's kernel reading its own source
  revisits depth if profiles complain. The known quadratic corners (child
  reversal per collection, per-close state clones) are O(n·depth) overall
  and fine at kernel-source scale; they are documented, not optimized.

### `dump.cato` — the differential serialization

One line per top-level form (byte-compare target; also the reference
driver's output):

```
line  := node "\n"
node  := "@" i64 ":" i64 "+" i64 tag          ; line:col+len of the form
tag   := "(sym " name ")" | "(int " i64 ")" | "(f64 " %g ")" | "(bool " 0|1 ")"
       | "(str " esc ")" | "(nil)" | "(list" node* ")" | "(vec" node* ")"
       | "(map" node* ")" | "(set" node* ")"
```

- `esc` escapes `\` `"` newline `\t` so a form stays on one line; spaces
  inside string values are printed raw (byte comparison only, no parser).
- `%g` is `(str f)` — the S3 number→string path, `%g` in both backends.
  Fixtures avoid floats where the interp's mimic (`fmt_f64`) and the C
  runtime's `%g` differ in edge formatting (nothing ≥ 1e6 or < 1e-4).
- Symbol names render through the intern table carried in `ROk`; the seed
  side has names only, so sids never cross the wire.
- Empty input produces empty output.

### `reader_ref.cato` — the reference driver

`(args)` supplies a path (index 0); the driver slurps it, reads it, and
prints either the serialization or `err @line:col+len msg` on one line —
the exact rendering the differential test compares against. Missing arg →
empty path → `slurp` returns `""` → empty output (no special case needed).

## Fixtures (`selfhost/cato/reader/fixtures/`)

Edge cases the examples corpus doesn't cover, each a byte-compare input:
`atoms.cato` (`+5` `-5` `1.5` `.5` `5.` `1.2.3` `true` `false` `:when` `nil`
`,`-symbol `#tag` `sym.with.dots`), `colls.cato` (empty and nested
`#{}` `{}` `[]` `()`), `strings.cato` (escapes incl. lenient `\x`, escaped
backslash, quote, multi-line strings), `comments.cato` (trailing, whole-line,
EOF, `;` inside a string), `err_unbalanced.cato`, `err_map_odd.cato`,
`err_unterminated.cato`, `err_unclosed.cato`. Malformed fixtures pin the
error messages and spans on both sides.

## Differential test (`crates/cato-backend-interp/tests/kernel_reader.rs`)

- **Seed oracle**: a Rust serializer over `cato_core::reader::
  read_all_spanned` — top-level byte spans rendered as
  `@line:col+len` (line = 1 + newlines before `lo`, col = `lo` − line start,
  len = `hi − lo`), the same node grammar, f64 through the interp's
  `fmt_f64` logic (the oracle and the interpreted kernel share one renderer;
  the compiled-kernel parity run covers the LLVM side).
- **Kernel under test**: the canonical concat (`lib` files, then `read`,
  `dump`, `reader_ref`) written to `target/`; run per file as `cato run
  <concat> <file>` — trailing positionals are the program's `(args)` (S3),
  so the driver reads the path from `(get (args) 0 "")`.
- **Corpus**: every file under `examples/` (recursive, including `errors/`
  and `modules/`) plus every fixture. Byte-identical stdout per file, or the
  test names the mismatching file.
- **Backend parity** (the S4 pattern): the same concat is emitted to LLVM,
  clang-compiled, and run over the fixtures — interp and native stdout agree
  byte for byte, so the reader is exercised on both backends from day one.

## Non-goals

- Per-node span comparison against the seed (the seed AST has none) —
  top-level spans plus the driver's hand-checked sample cover the counters.
- `quote`/syntax-quote, keywords, metadata, readers macros — not in the seed
  reader, not here.
- Recovering from errors (first error ends the read, like the seed).
- Performance: the documented quadratic corners are fine at S5 scale; S8
  profiles decide if they matter for the kernel reading itself.
