# Cato Ownership & Borrows design (Phase 7, bounded slice)

Cato has no heap yet, so this slice lands the **enforcement layer** with
zero runtime cost: the ABI of borrowed and owned params is byte-identical
(the callee copies the value at the boundary either way). What it buys is
the semantic contract that Phase 7's heap work will build on.

## Rules

- **Non-Copy types** (ADTs, vectors) **move** when passed by value, bound in
  a `let`, or stored in a constructor. Using a moved binding is a compile
  error (`use of moved value x`). Scalars (i64/f64/bool) are Copy.
- **`&ty` — shared borrow** (`(defn peek [b :- &Box] ...)`): the caller
  keeps the value; borrows are function-scoped, so they end when the call
  returns. A borrowed param is usable inside the callee.
- **`&!ty` — exclusive borrow**: same as shared, plus two exclusive borrows
  of the same binding in one call are rejected
  (`cannot borrow b exclusively more than once in the same call`).
- **`(clone x)`** — explicit copy. ADTs materialize a fresh stack slot and
  copy the struct by value; the source stays usable.
- **`(drop x)`** — ends a binding without a heap; using it afterwards is a
  use-after-move. `match` borrows the scrutinee, so
  `(match b (Box [v] v) ...)` doesn't consume.
- **Fused loops borrow their captures** — a reduce body referencing an outer
  vector/ADT runs per iteration and never consumes it.
- Branch merges are flow-sensitive: a value moved in *any* `if`/`match` arm
  counts as moved afterwards (soundness over precision).

## Checker

A flow-sensitive pass in `build_program` after lowering (and after
monomorphization, so instantiated generics are checked with their concrete
types). It tracks per-binding state (`Owned`/`Moved`) through `let`/`if`/
`match`/`seq` bodies and consults each callee's parameter borrow kinds —
builtins (`count`, `print-*`, `get`, `even?`, `clone`, `drop`) never
consume.

## Syntax note

Borrow sigils live in annotations: `&Box`, `&!vec`, and `&x :- (Vector f64)`
(name position) are accepted. Call sites pass plain values — the callee's
annotation decides move vs borrow.

## Deferred to the heap slice

The `noalias` extension to user pointers, escaping values, `conj`/`subvec`/
`for` (vector materialization), and runtime drop glue. The checker's move
rules are exactly what makes that step safe.

## Reference program

`examples/own.cato` → `123 3 7`.
