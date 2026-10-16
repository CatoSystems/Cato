# HANDOFF: Kernel Clone Elimination & Memory Churn Optimization

## Status
- Kernel self-build alloc churn: **50.56 GB → 4.09 GB (−91.9%)**;
  ALLOC_CALLS **1.19B → 114M (−90.5%)**; kernel self-build wall time
  **47.5 s → 5.2 s (−89%)**.
- Gate: PASS (fixpoint k1==k2==k3, t1–t6, bk1/bk3 ok, bk2/bk4 rejected).
- Tests: `cargo test` green apart from two known non-blockers —
  `cato-interp kernel_emit` (`interp: unbound var adts (fn b-retb)`, an
  interpreter scoping bug, pre-existing) and the `cato-cli` repl pty test
  (sandbox denies `openpty`: `script: openpty: Operation not permitted`).
- CPU: ~40% of the self-build is now the kernel *waiting for clang*
  (`__wait4_nocancel` samples); of the rest, allocation/free dominates
  (clone/drop glue), strlen is down from ~90% to ~4%.

## Round 4 results (parser state, 5.67 → 4.09 GB)
Same method: inclusive attribution per call site, fix the top site, verify
with the gate and with byte-identical IR from the pre/post compilers.
- `ps-failed` took the parser state by value and only read the error flag:
  24 call sites copied the whole state (ADT table, signature table,
  declarations) to keep their own copy. 5.26 → 4.71 GB.
- The remaining `ps-*` readers: `ps-tab` / `ps-str-of` / `ps-err-msg` return
  Copy values and now borrow; `ps-adt-at` / `ps-adts-depth` /
  `ps-adts-find-inst` / `ps-adt-inst-name` fuse a `(f (ps-adts (clone ps)))`
  read into one borrowed walk; `inst-name-loop` / `ty-mangle-str` copied the
  whole ADT table per type argument for a suffix. 4.71 → 4.09 GB.
- Narrow variant reads (`varnt-name-at`, `vlist-fields-at`, `variant-fields`
  borrowing its `AdtInfo`), `defn-ret-ll` borrowing the module, and the
  wildcard arm threading the definition list its lowering returned.
Negative result: flipping `DParams`/`TyEnv` parameters wholesale and letting
the checker reject the rest (the loop that worked for `TyEnv` in round 2)
now converges to almost nothing — the remaining owned consumers are real,
so those sites need fused accessors, not signature changes.

## Round 3 results (inclusive attribution, 19.34 → 5.67 GB)
The decisive tool: **inclusive per-call-site byte attribution**.
`tools/instr_clones.py` brackets every `clone__X(...)` call with
`__dbg_enter/__dbg_exit`, and `tools/dbg_hit.c` — built as a *dylib* and
injected, because interposition does not work from the main executable
(link the instrumented IR with `-Wl,-undefined,dynamic_lookup`) — adds each
allocation to the outermost bracketed call. One run ranks "how much churn
does this call site start", with no modelling guesswork:
```bash
./target/release/cato selfhost/cato.cato --emit-ir -o /tmp/k.ll
python3 tools/instr_clones.py /tmp/k.ll /tmp/i.ll /tmp/map.tsv
clang -dynamiclib -O2 -o /tmp/dbg_hit.dylib tools/dbg_hit.c
clang -O2 -Wl,-undefined,dynamic_lookup /tmp/i.ll runtime/runtime.c -o /tmp/ki
DYLD_INSERT_LIBRARIES=/tmp/dbg_hit.dylib DBG_MAP=/tmp/map.tsv \
  /tmp/ki build selfhost/cato.cato /tmp/k3 2>/tmp/sites.txt
sort -t$'\t' -k4 -rn /tmp/sites.txt | head -20   # SITE id calls bytes fn callee
```
What it found (each step verified by the gate and by byte-identical IR from
the pre/post compilers on the same input):
- **One call site was 38% of all churn.** `match-variant-arm` lowered an arm
  body and then passed a *clone* of the whole `Defns` list (every definition
  emitted so far, bodies included) to the next arm. Expression lowering never
  adds a definition, so the list the lowering returns is that same value;
  threading it removes the copy. (19.34 → 9.41 GB with the next item.)
- **`md-adt-name` cloned the whole ADT table to index one entry.**
- **Cloning `Emit`/`Mdl` to read one field** (5 sites) copied the globals
  StrBuf (every emitted line), the ADT table and the signature table; the
  `em-boxed-at` / `em-adt-name` borrowed readers fix it (9.41 → 7.41 GB).
- `adts-tparams` / `instance-of-template` borrowed (callers cloned the whole
  table and the callee dropped it); `varnt-fields-at` fused its
  AdtInfo/Varnt walk so only the small field list is copied (7.41 → 7.01 GB).
- `match-binders` threaded the ADT table and instance map and returned them
  untouched — callers cloned a whole `AdtTab` *and* an `IMap` per arm. The
  dead parameters are gone; the arm body's lowering threads what it returns.
  Plus `(adt-of scrut (clone st1))` and `(varnt-fields-at (b-adts-of bcx)
  ...)` copies (7.01 → 5.67 GB).
- `ps-err` now moves its fields instead of cloning them (the copies were
  already elided at codegen — a cleanup, not a measured win).

## Round 2 results (borrowed tables + destructuring, 21.73 → 19.34 GB)
Self-build wall time 21.4 s → 16.7 s (−22%); cumulative runtime −65%.
Worked:
- **Narrow `Mdl` accessors.** `md-adts`/`md-owns`/`md-csigs` return an owned
  table (a deep copy of the ADT table / ownership list / signature table)
  and were only ever called to feed a `&T` parameter. `md-fs-total`,
  `md-fs-off`, `md-fs-field-ty`, `md-esigs-at` and an inlined `md-owns-at`
  do the read inside the match. Same for `(b-borrowed-call v (b-retb-of
  bcx))` → `b-borrowed-call-of`.
- **`TyEnv` borrows.** 38 signatures take `&TyEnv`; the walkers that extend
  the environment keep it owned and clone only at the extension site.
- **Destructure instead of clone-then-split.** `(hes-first (clone args))`
  retires the chain box, so reading the tail afterwards copied the whole
  remaining list; matching the chain once removes both.
- `esigs-at` carried an unread `Mdl` parameter — dropping it lets a
  borrowed Mdl be searched without copying the signature table.

Tried and rejected (do not repeat without new evidence):
- **Blanket `&HExpr` on the emitter.** It cascades: the emitter walkers
  *consume* the expression and move its fields (arm lists, binders, bind
  lists) into owned consumers, so borrowing a parameter forces clones of
  those fields — trading one outer clone for many inner ones. Flipping all
  `HExpr` params in `emit/*.cato` and reverting what the checker rejects
  keeps only 6 functions. The same experiment on `TyEnv` worked because
  most type-env consumers are read-only probes.
- **`ESplit` (an ADT carrying head+tail).** The box costs more than the
  small-list clone it replaces (+46 MB); matching the list directly is the
  win.
- Borrowing the read-only predicate family (`all-paths-recur`,
  `validate-arms`, `arms-of` consumers) plus fused `-of` helpers: neutral
  (−0.1%) for ~40 lines.

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
Re-run the inclusive attribution (commands above) before choosing; at
4.09 GB the profile has shifted. Last measured leaders:
- `clone__Varnt <- vlist_at`: callers that take a whole `Varnt` to read its
  field list (`variant-index` returns it, `lower` reads fields at 2453/2571,
  `fill-variants` at forms 598). `vlist-fields-at` exists; the remaining
  cost is the field-list copy itself, so these need the *consumer* to accept
  a borrowed list or the field list to stop being needed twice.
- `clone__TyEnv <- lower_ctor_fields` / `match_variant_arm` /
  `lower_str_args`: owned `TyEnv` walkers. The whole-program flip converges
  to nothing (round 4 negative result) — fuse the read instead.
- `clone__AdtInfo <- adts_at`: `adts-at` hands back a whole `AdtInfo`
  (variants and field lists); callers that read one field want a narrow
  accessor (`adts-name-at`, `adts-tparams-at`, ...).
- `clone__Defns <- lower_cmp` and the other `(clone defs)` / `(clone insts)`
  sites in `lower.cato` error paths: check with inclusive attribution
  whether they are hot before touching them.
- `clone__HExpr` is now small but diffuse (small sub-tree copies at node
  boundaries); only targeted, measured edits move it — the blanket borrow
  conversion does not (see Round 2 negative results).
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
1. Fuse `dparams-get` with its two consumers (`b-call-move-ps`,
   `b-tail-args-ok`) so the param list stays borrowed; convert the
   remaining `(match st (Emit [c m s] (clone m)))` reads to `&Emit`
   accessors (targets `clone__StrCell` + `clone__PList`).
2. Chase the nested `clone__TyList` volume: find which remaining callers
   clone a whole `AdtTab`/`Sigs`/`AdtInfo` (entry sites are few — the
   instrumentation in the quick commands lists them), and shrink those.
3. Explore adding Cato-level leak guards (ownership semantics) to catch
   leaks at compile time.
4. Keep changes semantics-preserving; run the gate after each batch.
