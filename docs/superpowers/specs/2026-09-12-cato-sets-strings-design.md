# Cato Sets & String Building Design (Phase 11)

## Goal

Complete the collection story on top of the heap slice: immutable i64 sets
(`#{...}`, `conj`/`disj`/`contains?`/`count`) and heap string building
(`subs`, `split`, `join`).

## Sets

- Reader: `#{1 2 3}` is a first-class delimiter pair (`#` + `{...}`) and
  produces `Expr::SetLit`. Closing `}` accepts either a `map` or `set` kind
  on the delimiter stack.
- Representation: the same `{ptr, len, cap}` header as vectors/maps, with a
  flat sorted `i64` array on the heap. Literals are sorted and deduped at
  compile time, so a set literal is one malloc and straight stores.
- Immutability: `conj`/`disj` return fresh sets (filter-copy with a write
  pointer, exactly like `assoc`), so old sets stay valid — snapshot
  semantics, no in-place mutation.
- `contains?` is a branchless linear scan (`or`-accumulated `icmp eq`),
  matching the map `get` style — no basic-block forking, loop-friendly.
- Sets are Copy like maps (immutable snapshots never need move tracking).

## Strings

Strings are NUL-terminated `ptr`s (static literals or heap). New nodes:

- `(subs s start end)` — malloc + `memcpy` of the byte range + NUL. A fresh
  copy (same semantics as `subvec`), so the result outlives any source.
- `(split s sep)` — runtime helper `cato_str_split` returns a malloc'd array
  of malloc'd strings; the count is written through an out-param. The result
  binds as a `(Vector str)`.
- `(join sep xs)` — runtime helper `cato_str_join` over a `(Vector str)`
  produces one malloc'd NUL-terminated string.
- `(Vector str)` is a new vector element kind (`Elem::Str`): an array of
  `ptr`s. `get` on it is bounds-checked with `null` (or a default string)
  out of range; `count` reads the header length.

## Bool widening

A bool expression in an i64 context (e.g. `(print-i64 (contains? s 3))`)
now widens via `zext i1 → i64` instead of panicking — the printer then
shows an honest 0/1.

## Tests

- reader: `#{...}` parses as `SetLit`, nested in forms.
- hir: literal sort/dedup, op lowering, and rejection when the source is
  not a set / not a string.
- e2e: `examples/sets.cato` (exact output asserted) plus edge cases:
  empty parts from `split`, empty `subs`, default on out-of-range string
  `get`.
