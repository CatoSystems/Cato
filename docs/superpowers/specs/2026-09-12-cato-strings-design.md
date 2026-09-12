# Cato Strings design (Phase 8, bounded slice)

With the heap still pending (Phase 7b), the honest Phase 8 core is the
string layer built entirely on static data:

- `"..."` literals (the reader already parsed them) become `Ty::Str`
  values: private `[N x i8]` constants (NUL-terminated) with the value
  being a `ptr` to the first byte — one GEP, zero allocation.
- `Ty::Str` is **Copy** (immutable static data): it moves nowhere, and
  `&str` borrow params pass pointers directly.
- `(= s1 s2)` / `(/= s1 s2)` compare **content** via the C runtime
  (`strcmp`); ordering comparisons on strings are rejected at HIR time.
- `(count s)` is `strlen` (declared from the C runtime).
- `(print-string s)` joins the other print builtins in `runtime.c`.

Strings are rejected in arithmetic and never promote; comparisons with
mixed types are type errors as usual.

## Deferred to the heap slice (Phase 8b)

- HAMT maps (flat ≤8-entry struct first) and bit-packed sets — `assoc` /
  `conj` need allocation; the literal syntax `{}` / `#{}` will land with
  them.
- `(str ...)` concatenation, string building, copy-on-write slices.
