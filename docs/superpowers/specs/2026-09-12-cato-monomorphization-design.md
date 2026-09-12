# Cato Monomorphization design (Phase 6)

## Goal

Generalize `(Vector T)` and parameterize over element types: generic `defn`s
are instantiated per concrete call-site type, eliminating boxed dispatch.
Compile times grow by design — aggressive monomorphization is the chosen
trade for zero-cost runtime.

## Type system

- `Ty::VecI64` becomes `Ty::Vec(Elem)` with `Elem ∈ {I64, F64}`.
- Annotations accept `(Vector i64)` / `(Vector f64)` / `(Vector T)`; `vec`
  remains an alias for `(Vector i64)`.
- Any unknown type symbol in an annotation is a **type parameter** (e.g. the
  `T` in `(defn gsum [xs :- (Vector T)] :- T ...)`), indexed as
  `Ty::Param(i)` / `Elem::Param(i)` against the defn's ordered `ty_params`.
  Type parameters never survive lowering: templates are only lowered through
  instantiation.
- `(Vector T)` binds `T` to the **element type** (f64), not the vector type —
  so `:- T` returns the scalar element and `:- (Vector T)` returns a vector
  of the same element.

## Monomorphization

- Concrete defns lower exactly as before. Generic defns are templates.
- At a call site (including `map`/`filter` stage functions), the declared
  parameter patterns unify against the concrete argument types; unresolved
  parameters are a compile error.
- Each distinct type-argument tuple yields one specialization named
  `name__i64` / `name__f64` / `name__vec_i64` / ... , cached in an
  instantiation table so recursive generic calls resolve to the same body.
- Instantiation lowers the template body with the substituted parameter
  environment; nested generic calls instantiate recursively.
- Generic bodies with f64 element types follow the `if` promotion rule:
  i64 accumulators/inits promote (int literals directly, expressions via
  `sitofp`).

## Codegen

- Vector values stay `{ ptr, i64 len }` (the `%VecI64` struct name is kept
  for all element types — layout is identical); element loads/GEPs use the
  element LLVM type: `getelementptr inbounds double, ptr %data` and
  `load double` for f64 vectors, still `inbounds` + `align 8`.
- `(vec-f64 1.5 2.5)` mints `[N x double]` globals; loop helpers take one
  `data/len` pair per base vector regardless of element type.
- `get` on an f64 vector loads and selects `double`s, branchlessly as before.

## Deferred

- ADT fields holding vectors (needs heap/indirection — Phase 7).
- `(Vector bool)` and nested `(Vector (Vector T))` (no new capability in
  this slice; straightforward follow-ups).
- Generic type parameters in ADT definitions (`(deftype (Option T) ...)`).

## Reference program

`examples/generic.cato` → `10 2.5 7.25 9 2.5 50`, with `@gsum__i64`,
`@gsum__f64`, `@gmax__i64`, `@gmax__f64` defines in the emitted IR.
