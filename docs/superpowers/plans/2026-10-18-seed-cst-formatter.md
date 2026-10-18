# Seed CST Formatter (`cato format`) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `cato format <file>... [--check] [--width n]` — a seed-only, comment-preserving formatter built on a new full-fidelity CST (not the AST/HIR), rendered with a Wadler-style pretty-printer.

**Architecture:** New crate `crates/cato-fmt` with four layers: a lossless lexer (every byte becomes a token or trivia), a delimiter-matching CST parser (trivia slots preserved on every node), a classic Wadler `Doc` engine, and a policy layer that knows Cato's surface forms (`defn`, `let`, `match`, …) and decides breaks/indentation. `cato-core`'s reader is untouched. The CLI dispatches `format` early in `main` like `repl`/`lsp`.

**Tech Stack:** Rust, zero new dependencies (hand-rolled lexer/parser/printer), golden + property tests.

**Spec:** `docs/superpowers/specs/2026-10-18-cli-contract-design.md` §3 — the plan argues from it; executors read both.

## Global Constraints

- `cato-core`'s reader/AST must not be modified or even imported by `cato-fmt` (spec §3: the compiler parser must not risk regression for a formatting feature).
- No third-party dependencies (workspace is path-deps only).
- Formatter must be **total**: every byte of input becomes some leaf in the CST (lex error → format error, never silent drop); output always ends with exactly one newline.
- Never rewrite token text: string literal contents (escapes included) and atom spellings pass through byte-identically.
- Idempotency is a hard guarantee: `format(format(x)) == format(x)` (spec §3).
- Default width 100, indent unit 2 spaces, max one blank line between top-level forms — these are the policy's pinned defaults.
- Help text must list `format` per spec §1 (kernel shows "not yet available in cato1" until a later plan).

## Review Focus

1. **Comments in hostile positions must survive** (leading a form, trailing on a `)` line, inside a broken `let`, after the last form with no trailing newline). Test: golden fixtures in Task 4 place `;` comments at each of these positions; assertions check byte-exact output.
2. **A malformed file must fail loudly, not half-format** (unbalanced delimiters, unterminated string). Test: Task 4 error tests — `format_source` returns `Err` with the reader-equivalent message; CLI exits 1 and leaves the file untouched.
3. **Idempotency across the real corpus, not just fixtures.** Test: Task 4 property test runs `format(format(x)) == format(x)` over `examples/*.cato`, `selfhost/cato/**/fixtures/*.cato`, and `crates/cato-fmt/tests/golden/*.cato`.
4. **`--check` must not write anything** (CI mode). Test: Task 5 asserts file bytes are identical before/after a `--check` run on a badly-formatted file, exit code 1, summary on stdout.
5. **Width regressions: groups must actually break at the configured width.** Test: Task 3 Doc-engine tests assert exact rendered output for a 40-column case; Task 5 adds a CLI test with `--width 40`.

---

### Task 1: Lossless lexer

**Files:**
- Create: `crates/cato-fmt/Cargo.toml` (name `cato-fmt`, path-dep on nothing), `crates/cato-fmt/src/lib.rs`, `crates/cato-fmt/src/lexer.rs`
- Modify: workspace `Cargo.toml` (add `crates/cato-fmt` member)
- Test: `crates/cato-fmt/tests/lexer.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (Task 2 relies on these exact names):
  - `pub enum TokKind { Ws, Comment, Open /* ( [ { #{ */, Close /* ) ] } */, Str, Atom }`
  - `pub struct Token { pub kind: TokKind, pub text: String }` (`text` is the raw byte slice as String)
  - `pub fn lex(src: &str) -> Result<Vec<Token>, FmtError>` — `FmtError { pub msg: String, pub offset: usize }` (defined in `lib.rs`)
  - Lexing rules (ported from the seed reader, `crates/cato-core/src/reader.rs:34-197`, but retaining trivia): whitespace = space/`\n`/`\r`/`\t`; `;` → line comment through end of line (comment token excludes the newline); strings start `"` with backslash-escapes skipping the next byte (unterminated → `Err` with offset of the opening quote); atoms run until `" \t\n\r()[]{};`; `#{` lexes as one `Open` token.

- [ ] **Step 1: Write failing tests (`tests/lexer.rs`)**

```rust
#[test] fn concat_of_token_texts_equals_input()  // for every fixture: lex(src).unwrap().iter().map(|t| t.text).collect::<String>() == src
#[test] fn comment_is_one_token_to_end_of_line() // "; hi\n(x)" -> [Comment("; hi"), Ws("\n"), Open, Atom("x"), Close]
#[test] fn escaped_quote_does_not_end_string()   // "\"x\"" -> single Str token with text "\"x\""
#[test] fn unterminated_string_errors_with_offset() // offset == index of opening quote
#[test] fn set_literal_opens_as_one_token()      // "#{1}" starts with Open("#{")
```

Fixture corpus for `concat_of_token_texts_equals_input`: iterate `../../examples/*.cato` and `../../selfhost/cato/reader/fixtures/*.cato` (skip `err_*`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cato-fmt`
Expected: FAIL (crate missing / compile error)

- [ ] **Step 3: Implement `lexer.rs`** — single forward scan; classify atoms lazily (the lexer keeps them raw — classification is the parser's/policy's job).

- [ ] **Step 4: Run to verify passing**

Run: `cargo test -p cato-fmt`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cato-fmt Cargo.toml
git commit -m "feat(fmt): lossless Cato lexer with trivia tokens"
```

---

### Task 2: CST parser (trivia-preserving)

**Files:**
- Create: `crates/cato-fmt/src/cst.rs`
- Test: `crates/cato-fmt/tests/cst.rs`

**Interfaces:**
- Consumes: `lexer::{lex, TokKind, Token}`, `FmtError`.
- Produces (Tasks 3–5 rely on):
  - `pub enum Elem { Node(Node), Token(Token), Trivia(Trivia) }`
  - `pub enum TriviaKind { Ws, Comment }` with `pub struct Trivia { pub kind: TriviaKind, pub text: String }`
  - `pub enum NodeKind { List, Vector, Set, Map, Atom, Str }`
  - `pub struct Node { pub kind: NodeKind, pub open: Token, pub close: Option<Token>, pub kids: Vec<Elem>, pub text_len: usize }`
  - `pub enum Top { Node(Node), Trivia(Trivia) }`
  - `pub fn parse(src: &str) -> Result<Vec<Top>, FmtError>`
  - Invariants: every input byte appears exactly once across leaf texts (node `text_len` = sum of subtree leaves); trivia is a `Trivia` sibling **in position** (leading trivia before an element, trailing after); mismatched closer (`( … ]`) and stray closers are `Err`; `Map` nodes with odd kid count are `Err` (parity with the reader, `reader.rs:76-81`).

- [ ] **Step 1: Write failing tests (`tests/cst.rs`)**

```rust
#[test] fn tree_text_is_lossless()      // flatten(parse(src)) == src, over the same corpus as Task 1
#[test] fn comment_between_forms_is_trivia_sibling() // two defn Lists with Ws+Comment elems between
#[test] fn comment_inside_list_trailing_close()      // (a b ; note\n) -> Comment is last kid before close
#[test] fn mismatched_delimiter_is_error()           // "(]" -> Err mentioning delimiter
#[test] fn stray_close_is_error()                    // ")" -> Err
#[test] fn map_with_odd_elements_is_error()          // "{1 2 3}" -> Err "map literal must have key/value pairs"
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cato-fmt`
Expected: FAIL

- [ ] **Step 3: Implement `cst.rs`** — recursive descent over the token stream: on `Open`, push frame until matching `Close`; `Atom`/`Str` become leaf `Node`s with `kids: [Token]`. Trivia tokens are emitted as `Trivia` elems into the current frame (or top level). No classification of heads happens here.

- [ ] **Step 4: Run to verify passing**

Run: `cargo test -p cato-fmt`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cato-fmt/src crates/cato-fmt/tests
git commit -m "feat(fmt): full-fidelity CST with trivia slots"
```

---

### Task 3: Wadler `Doc` engine

**Files:**
- Create: `crates/cato-fmt/src/doc.rs`
- Test: `crates/cato-fmt/tests/doc.rs`

**Interfaces:**
- Consumes: nothing (pure).
- Produces (Task 4 relies on):
  - `pub enum Doc { Text(String), Line, // hard newline
      Soft, // space-or-newline inside a group
      Nil, Group(Vec<Doc>), Nest(i32, Box<Doc>), Concat(Vec<Doc>) }`
  - helpers: `pub fn text(s: impl Into<String>) -> Doc`, `pub fn group(d: Doc) -> Doc`, `pub fn nest(n: i32, d: Doc) -> Doc`, `pub fn concat(xs: Vec<Doc>) -> Doc`
  - `pub fn print(doc: &Doc, width: usize) -> String` — classic Wadler best-fit: measure flat width of a group (hard `Line` ⇒ infinite), print flat if it fits in `width - col`, else break: `Line` → newline + current indent, `Soft` → single space when flat / newline + indent when broken. `Nest` adds to indent for contained breaks.

- [ ] **Step 1: Write failing tests (`tests/doc.rs`)**

```rust
#[test] fn group_fits_flat()        // group("foo" Soft "bar") @ width 10 -> "foo bar"
#[test] fn group_breaks_when_too_wide() // same @ width 5 -> "foo\nbar" (col/indent 0)
#[test] fn nest_indents_broken_lines()  // nest(2, group(Text("aa") Soft Line)) @ width 1 -> "aa\n  " with trailing indent stripped -> "aa"
#[test] fn hard_line_forces_break()  // Concat[Text("a"), Line, Text("b")] -> "a\nb" regardless of width
#[test] fn width_40_regression()     // exact expected multi-line rendering of a 5-item group that fits at 40 but not at 20
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-fmt doc` → FAIL

- [ ] **Step 3: Implement `doc.rs`** — recursive `best(width, indent, col, items)` with the standard `fits` lookahead (flatten until first `Line`).

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-fmt doc` → PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cato-fmt/src/doc.rs crates/cato-fmt/tests/doc.rs
git commit -m "feat(fmt): Wadler Doc engine (group/nest/best-fit)"
```

---

### Task 4: Policy layer + `format_source` (golden + idempotency)

**Files:**
- Create: `crates/cato-fmt/src/policy.rs`
- Modify: `crates/cato-fmt/src/lib.rs` (add `pub fn format_source(src: &str, width: usize) -> Result<String, FmtError>`)
- Test: `crates/cato-fmt/tests/golden.rs` + golden fixtures

**Interfaces:**
- Consumes: `cst::{parse, Node, Elem, Top, NodeKind}`, `doc::*`.
- Produces: `pub fn format_source(src: &str, width: usize) -> Result<String, FmtError>` — CLI (Task 5) and the future LSP-formatting track call only this.

Pinned policy decisions (the spec fixes the substrate; these are the design's values):
- Top-level forms: source gap of exactly 1 newline → forms adjacent (one hard newline); source gap of ≥2 newlines → exactly one blank line between forms (the gap collapses, never grows).
- Inside lists/vectors/sets/maps: elements separated by `Soft`; after every element carrying a line comment, force a hard newline (a comment ends the line it sits on).
- Head-position atom recognized by text (`defn`, `deftype`, `extern`, `defgeneric`, `defmethod`, `let`, `match`, `fn`, `do`, `if`, `cond`, `when`, `for`, `->`, `->>`) gets policy treatment: binding/parameter vectors (`[...]` immediately after the head atom) stay in the head group; the body's first element starts on the same line when it fits, subsequent body elements each start a new line at `nest(2)`.
- `let` binding vector: group of per-binding `Soft` breaks — binds one-per-line when the whole group does not fit; each binding `[x v]` itself is a group.
- Long vectors/maps (no head): break after the open delimiter, one element per line at `nest(2)`, close delimiter on its own line at the outer indent.
- Empty list `()`, empty vector `[]` etc. print as-is (no inner break).
- Everything else: one `Group` per node (flat if fits, else element-per-line at `nest(2)`).
- Output ends with exactly one `\n`; input with no trailing newline gets one.

- [ ] **Step 1: Write golden fixtures + failing test (`tests/golden.rs`)**

Fixtures under `crates/cato-fmt/tests/golden/` as pairs `NAME.in.cato` / `NAME.out.cato` (at least): `defn_basic`, `comments_trailing`, `comments_leading_eof`, `let_long_bindings`, `match_arms`, `nested_vectors`, `blank_lines_collapse`, `strings_untouched` (string containing `;(` and escaped quotes), `map_odd_input_error` is an `Err` case (asserted inline, not a pair).

Test functions:
```rust
#[test] fn golden_files_format_exactly()   // format_source(read(NAME.in), 100) == read(NAME.out) for each pair
#[test] fn idempotent_on_golden_outputs()  // format(format(x)) == format(x) for each .out
#[test] fn idempotent_on_real_corpus()     // over examples/*.cato + selfhost fixtures (skip err_*): format twice equal, and format succeeds (parseable corpus)
#[test] fn malformed_input_is_error()      // "(defn f [" -> Err, message mentions unbalanced/open
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-fmt` → FAIL

- [ ] **Step 3: Implement `policy.rs`** — walk the CST into `Doc`; start with the generic rules, add head-specific rules for the pinned heads only (YAGNI: no rule beyond the list above).

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-fmt` → PASS (golden byte-exact + idempotency over corpus)

- [ ] **Step 5: Commit**

```bash
git add crates/cato-fmt/src crates/cato-fmt/tests
git commit -m "feat(fmt): Wadler policy for Cato — golden + idempotency over corpus"
```

---

### Task 5: CLI wiring — `cato format`

**Files:**
- Modify: `crates/cato-cli/Cargo.toml` (add `cato-fmt` path dep)
- Modify: `crates/cato-cli/src/main.rs` (early dispatch, like `repl`/`lsp`)
- Create: `crates/cato-cli/src/fmt.rs` (CLI glue only — arg parsing + file IO + messages)
- Modify: `crates/cato-cli/src/cli.rs` (`COMMANDS` + `usage_text()` gain `format`)
- Test: `crates/cato-cli/tests/format_cli.rs`

**Interfaces:**
- Consumes: `cato_fmt::format_source(src, width) -> Result<String, FmtError>`.
- Produces: contract behavior (spec §1/§3): `cato format <file>... [--check] [--width <n>]`, `cato format -` reads stdin → stdout; `--width` default 100, range 20..=1000 else exit 2; unknown flag → exit 2 with did-you-mean; per-file parse failure → `cato: <file>: <msg>` on stderr, continue remaining files, final exit 1; `--check` writes nothing, prints each would-change file path to stdout, exit 1 if any change else 0; success (rewrite mode) prints nothing.

- [ ] **Step 1: Write failing tests (`tests/format_cli.rs`)**

```rust
#[test] fn format_rewrites_in_place_and_is_quiet()  // badly-formatted file -> rewritten, stdout+stderr empty, exit 0, second run changes nothing
#[test] fn check_mode_reports_and_writes_nothing()   // bytes before == bytes after; stdout lists the path; exit 1
#[test] fn check_mode_clean_file_exits_zero()     // already-formatted file -> exit 0, empty stdout
#[test] fn width_flag_controls_breaking()            // --width 40 output == format_source(src, 40)
#[test] fn stdin_dash_formats_to_stdout()           // echo src | cato format - -> formatted stdout, exit 0
#[test] fn missing_file_exits_one()                  // cato format nope.cato -> exit 1, stderr has "cato: " and "nope.cato"
#[test] fn help_lists_format()                       // cato help contains "format"
#[test] fn malformed_file_exits_one_and_keeps_other_files() // two files, first malformed: exit 1, second still formatted
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-cli --test format_cli` → FAIL

- [ ] **Step 3: Implement** — `main.rs`: `if args.first() == Some("format") { fmt::run(&args[1..]); return; }` beside the `repl`/`lsp` dispatches. `fmt.rs` parses its own flags (format is not part of `parse_args`). `cli.rs`: add `"format"` to `COMMANDS`, add the spec §1 `format` line to `usage_text()`.

- [ ] **Step 4: Run to verify passing**

Run: `cargo test --workspace`
Expected: PASS (entire workspace, format tests included)

- [ ] **Step 5: Commit**

```bash
git add crates/cato-fmt crates/cato-cli
git commit -m "feat(seed): cato format — CST formatter CLI (--check/--width/stdin)"
```

---

### Task 6: Acceptance pass (spec §3)

- [ ] **Step 1: Corpus sweep**

Run: `cargo test -p cato-fmt && cargo test -p cato-cli format`
Expected: PASS — includes idempotency over `examples/` + `selfhost/**/fixtures/`.

- [ ] **Step 2: Manual spot-check**

Run: `cargo run -p cato-cli -- format examples/slice.cato --check`
Expected: prints the path, exit 1 or 0 depending on its current state; run without `--check`, then `git diff` shows only whitespace/comment-preserving changes — verify a `; comment` survives by inspection; run again → `--check` exits 0 (idempotency).

- [ ] **Step 3: Constraint grep**

Run: `rg 'cato_core|cato-core' crates/cato-fmt/`
Expected: no matches (formatter independent of the compiler parser).

Run: `cargo test --workspace`
Expected: PASS.
