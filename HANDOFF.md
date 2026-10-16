# HANDOFF: Kernel Clone Elimination & Memory Churn Optimization

## Status
- Kernel self-build alloc churn: **50.56 GB → 21.73 GB (−57.0%)**;
  ALLOC_CALLS **1.19B → 548M (−54%)**; kernel self-build wall time
  **47.5 s → 21.4 s (−55%)**.
- Gate: PASS (fixpoint k1==k2==k3, t1–t6, bk1/bk3 ok, bk2/bk4 rejected).
- Tests: `cargo test` green apart from two known non-blockers —
  `cato-interp kernel_emit` (`interp: unbound var adts (fn b-retb)`, an
  interpreter scoping bug, pre-existing) and the `cato-cli` repl pty test
  (sandbox denies `openpty`: `script: openpty: Operation not permitted`).

## How to see where the churn/CPU goes (do this first)
Deterministic, byte-stable, no MAXRSS noise:
- `tools/kernel_gate.sh` — ALLOC_BYTES/ALLOC_CALLS for the k2 self-build.
- `tools/alloc_profile.c` → per-call-site bytes/calls (`ALLOC_PROFILE`).
- `tools/alloc_stack.c` → byte-weighted *stack* samples (`ALLOC_STACK`);
  4 frames deep, enough for `strdup <- cato_str_vec_copy <- sb_str`.
- `tools/instr_clones.py` + `tools/dbg_hit.c` → exact per-call-site counts
  for every `clone__*`/`drop__*` call in an emitted `.ll` (the ground truth:
  the entry-vs-recursive split is what identifies the real cost driver).
- `sample <pid> 10 -file out.txt` → CPU call graph; this is how the
  `strlen`-per-byte reader cost was found (~90% of samples).

## What was fixed (all verified semantics-preserving)
1. **Reader `strlen` per byte (CPU)** — `(nth s i 0)` bounds-checks with a
   `strlen`, so every byte-at-a-time walk was quadratic. Added
   `cato_str_byte` (runtime + interp) and used it in the reader walks that
   already carry the length. 47.5 s → 39.1 s.
2. **`facts-closure` Warshall** — cell-wise probes over cons-list rows made
   the closure O(n⁴) (154 deftypes). Rewritten as row absorption (O(n³)),
   verified by diffing the kernel IR from the pre/post compiler: identical.
   39.1 s → 34.9 s, 50.56 GB → 49.63 GB.
3. **Borrow read-only state** — the biggest single win. `fs-*` layout
   helpers took `adts :- AdtTab`/`owns :- Sids` by value and deep-cloned the
   whole ADT table per recursion step (296k entry clones × ~147 nodes);
   `(clone ps)`/`(ps-failed (clone ps))`, `fc-*`/`esc-*` accessors (a FnCtx
   clone deep-copies the accumulated StrBuf), `tys-eq`/`sigs-find`
   (the `(clone (clone params))` double copy), `adts-find-inst`, `md-*`.
   `tools/borrow_drop.py` automates the argument side; signatures are
   hand-set. 34.9 s → 25.2 s, 49.63 GB → 35.18 GB.
4. **Loop accumulators grow in place (LLVM backend)** — two bugs:
   (a) `emit_conj` disabled the `len < cap` fast path whenever the planner
   did not prove a last use, so every append re-copied (quadratic);
   (b) the `recur` store rebuilt the vector header with `cap = len`, so the
   capacity was destroyed every iteration and the fast path could never
   fire. Fixes: `c.recur_fast` marks `(recur (conj v x) ...)` self-updates
   (sound because the slot is rebound to the append's result and every read
   of the old header keeps the pre-append length; aliased loop seeds and
   second conj sites stay copying), capacity is carried through vector
   values, and the old buffer is only freed under the planner's last-use
   verdict. Micro-benchmark 2000 appends: 36.0 MB/2.00M allocs →
   70 KB/4.25k allocs with identical output. 25.2 s → 21.4 s,
   35.18 GB → 21.73 GB.

## Remaining work (measured, in order)
- `clone__TyList` 5.19 GB and `clone__HExpr` 4.82 GB — now hundreds of
  small entry sites (thousands of calls each), not one culprit:
  `match-binders`, `call-marshal`, `lower-ctor-fields`, `emit-field-store`,
  `arms-of`, `ctor-store-args`, `md-owns-at`, `sigs-at`. Same medicine:
  `&T` params on read-only helpers. `md-owns-at` is the cheapest remaining
  one (it clones the whole `owns` Sids list just to index it).
- `strdup <- clone__StrCell` 1.39 GB: cloning a `FnCtx`/`Mdl` deep-copies
  its `StrBuf` (every emitted line). `fc-*`/`md-*` are borrowed now; the
  remaining sites pass whole `Emit`/`Mdl` values (`(match st (Emit [c m s]
  (clone m)))` in `emit.cato`/`heap.cato`) and could read the field through
  `&Emit` accessors instead.
- `clone__PList` 0.77 GB (`dparams-get`), `clone__TyEnv` 0.88 GB
  (`lower`, `lower-call-args`, `b-kids`, `unbound-param-2`).
- Cato-level leak guards remain unexplored (see the original note below).
- Goal: approach seed behaviour/resource profile.

## Files to focus
- `tools/borrow_drop.py` (argument side of the borrow conversion),
  `tools/instr_clones.py`, `tools/alloc_stack.c`, `tools/alloc_profile.c`
- `selfhost/cato/emit/state.cato`, `emit/*.cato`, `hir/lower.cato`,
  `hir/moveck.cato` (hotspots)
- `crates/cato-backend-llvm/src/llvm/heap_vec.rs`,
  `exprs_flow.rs`, `funcs.rs` (growable-slot codegen)
- `crates/cato-core/src/hir/ownership.rs` (planner) if adding leak rules

## Quick commands
```bash
python3 tools/concat_kernel.py && ./tools/kernel_gate.sh | tail -14
DYLD_INSERT_LIBRARIES=/tmp/alloc_count.dylib /tmp/k1 build selfhost/cato.cato /tmp/k2 2>&1 | grep ALLOC
# exact per-call-site clone counts:
./target/release/cato selfhost/cato.cato --emit-ir -o /tmp/k.ll
python3 tools/instr_clones.py /tmp/k.ll /tmp/i.ll /tmp/map.tsv
clang -O2 -c -o /tmp/dbg_hit.o tools/dbg_hit.c
clang -O2 /tmp/i.ll runtime/runtime.c /tmp/dbg_hit.o -o /tmp/ki
DBG_MAP=/tmp/map.tsv /tmp/ki build selfhost/cato.cato /tmp/k3 2>/tmp/hits.txt
python3 -m pytest tools/tests -q && cargo test
cargo build && cargo build --release   # the gate uses target/debug/cato: rebuild BOTH
```

## Verification discipline that worked
For a kernel-source change: seed compile-check → build k1 with the seed →
`cmp` the IR that the **pre-change compiler** and the **post-change
compiler** emit for the *same* input (byte-identical ⇒ semantics-preserving)
→ gate. For an emitter change: gate fixpoint + fixtures + `cargo test`
(parity suites) + a focused micro-benchmark with identical output.

## Next agent todo
1. Convert the remaining read-only accessors (`md-owns-at`, `sigs-at`,
   `Emit`/`Mdl` field readers, `dparams-get`, `TyEnv` walks) to borrows;
   measure after each batch with the gate.
2. Explore adding Cato-level leak guards (ownership semantics) to catch
   leaks at compile time.
3. Keep changes semantics-preserving; run the gate after each batch.
