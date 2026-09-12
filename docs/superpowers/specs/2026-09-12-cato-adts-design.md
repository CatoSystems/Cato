# Cato ADTs — `deftype` + `match` design (Phase 3)

## Goal

Unlock `Option`/`Result`/enum-style data with the core functional primitive:
algebraic data types. Scalars-first cut: payload fields are `i64`/`f64`/`bool`
only; ADT-valued and `(Vector T)` fields wait for monomorphization
(Phase 6) and indirection (Phase 7).

## Surface syntax

```clojure
(deftype Shape
  (Circle [radius :- f64])
  (Rect [w :- f64 h :- f64])
  (Point []))
```

- Each variant declares its payload fields as `name :- ty` triples in a vector.
- Every variant name becomes a constructor function: `(Circle 2.0)`,
  `(Point)`. Constructors shadow nothing; they are resolved before user calls.
- Fieldless variants take no arguments.

## `match`

```clojure
(match s
  (Circle [r] (* 3.14159 r r))
  (Rect [w h] (* w h))
  (Point [] 0.0))
```

- Each arm is `(Variant [binders...] body)`; binder count must equal the
  variant's field count and binders take the field types.
- A final arm `(_ body)` is a wildcard covering all remaining variants.
- Coverage is checked at HIR build time: without a wildcard, every variant
  must appear exactly once (duplicate arms are an error).
- All arm bodies must produce the same type (`i64`/`f64` promotion follows the
  `if` rule; ADT results merge as pointer phis).

## HIR

- `Ty::Adt(u32)` — index into an `AdtDef` table produced by `build_program`
  (`AdtDef { name, variants: Vec<(String, Vec<Ty>)> }`).
- `HExpr::Ctor { adt, variant, args }` and `HExpr::Match { scrut, arms, ty }`
  with `HArm { variant: Option<u32>, binders, body }` (`None` = wildcard).

## LLVM representation

Tagged union, scalars-first, unboxed, no heap:

```
%Shape = type { i64, [N x i64] }   ; tag + N = max field count
```

- Every payload slot is one 8-byte `i64` word. `f64` fields are stored with
  `bitcast double → i64` and loaded back with the inverse; `bool` is `zext`
  on store and `trunc` on load.
- Constructor: one `alloca` hoisted to the function entry prelude (loop-safe),
  tag store + field stores. The value is the `ptr`.
- `match`: load the tag, `switch` to arm blocks (default `unreachable`,
  since exhaustiveness is checked in HIR), load fields via `inbounds` GEPs.
- Arm results merge in a phi in the merge block: scalar results phi as
  `i64`/`double`/`i1`, ADT results phi as `ptr` (option→option transforms).
- Function signatures pass ADTs **by value** as `%Name`; the callee copies the
  incoming struct into an entry slot and the caller copies the result slot out
  on return. Internal representation stays ptr/phi-based; the by-value
  boundary prevents returning dangling pointers to callee stack slots — and
  stays off the heap.

## Non-goals

- Nested/recursive ADT payloads beyond scalars and other ADTs by value
  (recursive types need indirection → Phase 7 ownership).
- Pattern destructuring beyond one-level binder vectors.
