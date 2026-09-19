# S4 — Compiler stdlib in Cato (`selfhost/cato/lib/`)

Date: 2026-09-19
Goal: the compiler's own data structures — symbol tables, output buffers,
spans, AST — exist as ordinary Cato, built by the seed, parity-guarded.
Everything in this slice is library code: no seed-builtins beyond S0–S3, no
compiler hooks. Reference driver: `selfhost/cato/lib/stdlib_ref.cato`.

## Ground rules

- The lib is one logical namespace split across files for editing; the
  build concatenates them in a fixed canonical order. No `require` — the
  kernel subset avoids multi-file programs until after the fixpoint — so
  sibling files never redefine each other's names, and the driver defines
  the only `main`.
- From S4 onward `selfhost/` is built by the seed in CI (`cargo test`): a
  compile check per file and a byte-identical interp↔LLVM parity run over
  the concatenated driver.
- Loop variables are Copy-only, so every accumulating traversal is
  recursive. Last-use `conj` grows in place, which makes tail-recursive
  builders amortized O(1) per append — the shape every pass in S6+ uses.

## Modules

### `sym.cato` — symbol interning

There are no global variables in Cato, so the intern table is explicit
state threaded through calls.

- `(deftype Intern (I [tab :- (Vector str) next :- i64]))` — the table
  plus the next fresh sid (sids are table indices).
- `(intern2 st s)` → `(IRes st sid)`: first match wins; a miss appends
  and bumps `next`. `IRes` carries the *updated state* (not the raw
  table) so callers keep threading one value; `ires-st`/`ires-sid` read
  it back (each via `clone` — see the ownership rule below).
- `(sym-name st sid)` → `str`, `(intern-count st)` → `i64`,
  `(intern-tab st)` → `(Vector str)`.
- Names avoid LLVM-unsafe characters: no `->`/`?` in selfhost identifiers
  (`sp-str`, not `sp->str`).

### `strbuf.cato` — string builder

Emitter output and diagnostics accumulate pieces and join once; string
concatenation in a loop is quadratic and never appears in kernel code.

- `(deftype StrBuf (SB [parts :- (Vector str)]))`.
- `(sb-new)`, `(sb-add b s)`, `(sb-add-line b s)` (appends `s` then `"\n"`),
  `(sb-str b)` = `(join "" parts)` — the one join,
  `(sb-count b)` = piece count, `(sb-len b)` = total byte length.

### `span.cato` — source positions

- `(deftype Span (Span [line :- i64 col :- i64 len :- i64]))` — a value
  type (unboxed; i64 fields only). File identity is a future sid, not a
  field, until the interner and the reader meet in S5.
- `(sp-line s)` / `(sp-col s)` / `(sp-len s)` accessors,
  `(sp-str s)` rendering `line:col+len`.

### `ast.cato` — the reader's target AST (recursive — S1 exercised)

The shape S5's reader will produce; byte spans ride on every node.

- `(deftype Ast
    (ANil [at :- Span])
    (ASym [sid :- i64 at :- Span])
    (AInt [v :- i64 at :- Span])
    (AF64 [v :- f64 at :- Span])
    (AStr [v :- str at :- Span])
    (AList [kids :- Asts at :- Span])
    (AVec  [kids :- Asts at :- Span])
    (AMap  [kids :- Asts at :- Span])
    (ASet  [kids :- Asts at :- Span]))`
  with `(deftype Asts (KNil) (KCons [kid :- Ast next :- Asts]))`.

  Child collections are a chain, not a `(Vector Ast)`: the seed's field
  grammar allows vector elements over scalars and type parameters only,
  and the kernel subset must be written in what it can express — the same
  reason environments are assoc-lists. The Ast→Asts mutual recursion
  makes every node heap-boxed with ownership-driven drop glue (the first
  real workout for S1); spans are unboxed inline children. The reader
  (S5) builds children by persistent prepend — O(1) per node.
- Constructors `(ast-sym sid line col)` … fix the `Span` in place;
  `(ast-cons kid next)` builds chains.
- `(ast-count a)` — nodes in the tree; `(ast-tag a)` — a small
  discriminant for tests; `(asts-first ks)`/`(asts-depth ks)` — chain
  reads; `(ast-child-count a)` — chain depth per collection node.

### `alist.cato` — sid-keyed environments

The kernel's scope tables map sid → arbitrary value. Maps are i64→i64 only,
and a persistent linked list is the representation the kernel will keep, so
environments are assoc-lists from day one.

- `(deftype (AList V) (ALNil) (ALCons [k :- i64 v :- V next :- (AList V)]))`.
- `(al-put e k v)` — persistent extend (shadowing is replace-by-order),
  `(al-get e k d)` — scan with explicit default,
  `(al-has e k)` — membership,
  `(al-depth e)` — length, for tests.
- Generic over `V` (`str`, `(Vector str)`, ADTs — the mangle fix covers
  every instantiable type argument). No no-argument generic functions:
  type parameters bind from call arguments only, so the empty env is
  written as an ascribed `(ALNil)`.

### `result.cato` — error plumbing

- `(deftype (Result V) (Ok [v :- V]) (Err [msg :- str]))`.
- `(unwrap-or r d)`, `(is-ok r)`, `(err-msg r)` — the `Option`-adjacent
  contract the kernel's compile phases use: absence/failure is a value
  you match on.

## The extraction rule

The slice forces one ownership idiom into the open, now library law:
**a match-bound field aliases the box, and the box's drop glue frees the
field on function exit.** Returning a field, storing it elsewhere, or
letting a callee consume it therefore requires `(clone ...)` on
extraction (`trees.cato`'s `(sum (clone l))`, applied consistently).
Three seed bugs fell out of enforcing this end to end, each with its own
fix in the seed: `ty_of` typed `let` bodies without the bindings;
`tail_escape` missed `if`/`match` tail positions (scope-exit frees fired
on returned fields); and `check(Ctor)` did not move Var arguments into
owning boxes. The LLVM clone glue also had to learn owned vector fields
(deep copy on clone, `cato_str_vec_free` on drop) and field storage had
to heap-copy static literal vectors.

## Driver and tests

- `stdlib_ref.cato` — the reference program: interns a handful of symbols
  (including a duplicate), renders spans, builds a report through `StrBuf`,
  puts/gets over sid-keyed assoc-lists of strings and vectors, and walks a
  hand-built `Ast` counting nodes. Its exact output is the acceptance
  artifact for interp↔LLVM parity.
- `crates/cato-backend-interp/tests/selfhost.rs` — the canonical-order
  concat is built by the seed, and interp stdout == clang-compiled LLVM
  stdout, byte for byte. The driver's output is ASAN-clean under
  `-fsanitize=address` (checked during development; not in CI).

## Non-goals

- Hash-based interning (linear scan is fine at kernel-table sizes; the
  sid-keyed assoc-lists keep semantics honest until maps generalize).
- Mutable builders: `conj` on a provable last use already grows in place,
  so the API stays persistent with no performance asterisk.
- The reader itself (S5), macro expansion and typing (S6), any `require`.
