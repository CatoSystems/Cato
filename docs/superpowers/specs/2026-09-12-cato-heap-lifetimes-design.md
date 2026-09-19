# Cato Heap Lifetimes Design (Phase 13)

## Goal

Close the interim "allocations live until process exit" contract: heap
bindings are released at binding scope exit, soundly under the ownership
checker, with no double-frees or use-after-frees.

## The soundness problem

Naive scope-exit frees are unsound in Cato today for two reasons:

1. **In-place `conj`.** `conj` on an owned vector appends in place when
   capacity allows, so a `conj` result *aliases* its source's buffer (or,
   after growth, the source's header points to freed memory). Freeing both
   source and result double-frees.
2. **Return escape.** Vectors of generic functions can be returned
   (`%VecI64` by value); a callee must not free a binding the caller
   receives.

## Design: annotated, conservative frees

> **Status update.** The shape-based `annotate_frees` pass is superseded by the
> ownership-driven `plan_drops` (see `2026-09-18-cato-ownership-model-revised.md`):
> drops are derived from per-binding buffer ownership through the move checker, and
> moved values transfer their drop duty. The rules below are the historical design.

A frontend pass (originally `annotate_frees`, in the ownership module) walks each
defn and annotates every `HIR::Let` with a `frees` list. A binding is
included only when *all* of the following hold:

- its type owns a heap buffer: `Vec`, `Map` or `Set` (immutable snapshot
  families never alias — `assoc`/`conj`/`disj` copy);
- it is never passed as a call argument (an owned param may `conj` the
  buffer in place inside the callee, aliasing it) and never used as a
  `conj` source;
- it is not the function's returned value (the tail-`Var` chain);
- its name is not shadowed by another binding in the same defn (bindings
  share a codegen env slot).

Everything else leaks as before — the pass errs toward leaking, never
toward freeing twice. This frees all map/set literals and fresh,
un-shared vectors in typical programs.

## Emission

The LLVM backend emits, at each `let` scope exit (after the body value is
computed, inside the current block — so frees inside `if` branches happen
at branch exit), `load ptr` from the binding header + `call void @free`.
The interpreter needs no change: `Rc` releases at last use.

## Drop glue & noalias (deferred pieces)

ADTs currently hold only scalar fields, so there is no drop glue to
generate; it lands together with ADT fields that own heap values. The
`noalias` extension to user-owned pointers requires the uniqueness proof
above to be per-point rather than per-binding; deferred with it.

## Verification

- `scope_exit_frees_are_sanitizer_clean`: heap/maps/sets/slice examples
  compiled and run under AddressSanitizer — no errors, no leaks.
- `non_escaping_bindings_get_free_annotations`: HIR-level assertion that
  map/set bindings carry free annotations.
- The whole examples/ suite (incl. backend parity) stays green.
