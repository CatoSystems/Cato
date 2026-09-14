# Cato Parallel Loops Design (Phase 18)

## Goal

`(parallel (reduce + 0 xs))` chunks a provably order-independent fused
loop across a small pthread pool: same result, multiple cores, no GC, no
runtime dependency beyond pthreads.

## Qualification (compile-time)

`(parallel ...)` wraps exactly one `reduce` and is accepted only when the
fold is provably chunkable:

- the combine is `+` or `*`;
- the init is the identity literal for that op (`0` / `1`, int or float);
- the source is a plain vector variable (map/filter stages are fine —
  they are per-element pure);
- every capture is a scalar or string.

Anything else is a compile error with a targeted message ("requires + or
* ... with the identity init"). Non-commutative folds (closure reduces
like `-`) are rejected: partial-order changes would change results.
f64 results may reassociate (documented; the parity example uses exact
integer-valued sums).

## Emission

The existing fused-loop helper **is** the chunk function: it reads
`data[0..len]` from its `(ptr, i64)` params, so a chunk is just a sliced
pointer plus a length. Two extra functions per parallel loop:

- `@H_tramp(ptr arg)` — pthread entry: unpacks the arg record
  `{data, start, end, result-slot, caps...}`, calls `@H(data + start,
  end - start, caps...)`, stores the partial. Chunk ends clamp up to
  start so trailing workers with empty chunks see count 0.
- `@H_par(data, len, caps...)` — splits `[0,len)` into `--threads N`
  contiguous chunks (`ceil(len/N)`), mallocs one arg record per thread,
  `pthread_create`s the pool, joins (handle loaded by value — pthread_join
  takes `pthread_t`, not a pointer to it), combines the partials with the
  same op from the identity, frees the records.

Call sites of qualified reduces route to `@H_par`; sequential reduces are
untouched. `--threads N` (default 1) is a pure execution knob — output is
identical for every N (the test sweeps 1..8 and asserts equality).

The interpreter treats `parallel` as a sequential reduce (same values;
representation and scheduling are invisible to semantics).

## Auto-parallelization (deferred)

Auto-marking provably-disjoint fused loops (map/filter writes by index;
reduce needs the associativity proof) is a follow-up: the qualification
machinery above is exactly the check, it just needs to run without the
explicit `(parallel ...)` wrapper.

## Verification

`examples/parallel.cato`: sequential vs parallel `+`, wrapping `*`
(50! in i64 — identical partial-fold due to associativity+commutativity
under wrapping), and an f64 map+reduce — asserted identical across
1/2/4/8 threads and vs the interpreter.
