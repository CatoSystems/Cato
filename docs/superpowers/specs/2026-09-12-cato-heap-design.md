# Cato Heap design (Phase 7b)

Per user decision: **fold + in-place growth** — no malloc-per-operation.

## Representation

`%VecI64 = { ptr, len, cap }` — the previously-unused third field is now
capacity. Static (literal) vectors carry `cap == len` and point to
read-only global data. Runtime-built vectors are **growable bindings**:
an entry alloca slot holding the live `{ptr, len, cap}` header, read
through at every use (count, get, loop captures, call args).

## Allocation strategy

1. **Compile-time folding** — `conj`/`subvec`/`str` over literal data with
   constant arguments evaluate during HIR lowering into new static
   constants. Zero runtime cost; covers the common literal-pipeline cases.
2. **In-place growth** — `(conj v x)` consumes `v` (a move, enforced by the
   Phase 7 checker; in-place mutation is exactly what single ownership
   buys). Fast path: `len < cap` → one element store + len bump, no
   allocation. Growth path: capacity doubles (`max(2*cap, len+1)`),
   memcpy, free the old buffer (only when the source was heap-owned —
   static data is never freed).
3. **One allocation per materialized result** — `subvec` (clamped slice
   copy, branchless selects), `str` concat (sum of lengths + NUL), and
   `for` comprehensions (fill loop) allocate exactly once each.

## `for`

`(for [x src] body)` fills a fresh heap buffer with the body's result per
element. The body must be control-flow-free (checked in HIR) so the fill
loop is single-block; with `--fast` it carries the vectorize metadata like
fused reduce loops.

## Lifetime contract

Heap buffers live until process exit (or are freed by growth). Because the
move checker guarantees single ownership and heap construction cannot
appear inside fused loops (loop bodies are scalar-only), the number of live
allocations is bounded by static program size, not by data volume — there
is no unbounded leak, no use-after-free, and no double-free by
construction. Scope-exit frees are the follow-up once lifetimes are
tracked per-binding.

## Functions returning vectors

`defn`s with `Ty::Vec` returns pass the `%VecI64` struct by value at the
call boundary; the caller's binding becomes an owned (read-only or
subsequently growable) value.

## Reference program

`examples/heap.cato` → `3 8 30 4 10 heapalloc 9 3 9`.
