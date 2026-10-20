# Memory model v3 — progress

Status of the memory-efficiency / ownership work (design and evidence:
`docs/superpowers/specs/2026-10-19-cato-memory-model-v3.md`, Part V).

## Goal

Cut compiler memory and allocation churn without a GC, keep pointers and
low-level primitives, keep self-hosting: the seed (Rust, `crates/`) and the
kernel (self-hosted Cato, `selfhost/cato`) must plan drops by the same rules,
the kernel must reach its fixpoint (k2.ll == k3.ll), and every change is
measured.

Terms: **k1** = kernel built by the seed (seed's planner + LLVM backend);
**k2** = kernel built by k1 (kernel's own planner + emitter).

## Measurements (kernel self-compile, `tools/mem_bench.sh`)

| | peak live | churn | allocs | wall |
|---|---|---|---|---|
| k1 baseline | 212 MiB | 3.80 GB | 101.5 M | 3.34 s |
| k1 now | 115 MiB | 3.68 GB | 96.5 M | 3.45 s |
| k2 baseline | 3463 MiB | — | 102 M | 3.3 s |
| k2 now | 167 MiB | 4.12 GB | 109.7 M | 4.0 s |

k2 history: arm-exit frees 1827 → branch frees 229 → prefix merges 223 →
loop rules 167 MiB. k1/k2 IR identical; the gate's ASAN stage is clean.

## Done

Seed (Rust):
- Stage 1: `recur` is a scope exit (`HExpr::Recur(args, drops)`), owned loop
  variables (`loop_owning` fixpoint), loop-exit drops, arm-exit frees at the
  jump, `div_moved`.
- Stage 3: inferred parameter modes (`hir/infer.rs`, worklist + clone
  elision); value-flow use-after-free fixes (`move_value_roots`, let
  value roots).
- Stage 2: `:- (own str)` extern results; NULL `char*` reads as `""`;
  fresh operands of read-only builtins are freed.
- Types: arity/type checks (E201/E202), contextual numeric literals,
  narrow vectors (`(Vector u8)` etc.), integer ops (rem/mod/bit ops/shifts),
  division-by-zero trap.
- Fixes: real ASAN (`sanitize_address` on `.ll`), closures (captures,
  returns), OOB `get`, `subvec`/`args` values.

Kernel (`selfhost/cato`), Stage 4 so far:
- Arm-exit frees (`hir/alias.cato`), branch-local frees and a full watch
  stack, prefix-only state merges (`hir/statemap.cato`).
- Loop rules (`hir/loops.cato`): owned loop variables, recur hand-over and
  drop lists guarded by a may-alias roots analysis, loop-exit drops, arm
  plans on recurring arms; emitter runs pending arm exits and drops at
  `recur` after evaluating all arguments.
- Context: the checker threads a `Scope` (watches, iteration frame, roots,
  in-place accumulators) instead of rebuilding `BCtx` per loop/recur arg.
- Value-flow moves (`move_value_roots`): a non-Var owned argument (an
  if/match/let/seq merge, or any constructor field) hands over every
  non-Copy binding it may be — each tail leaf that is a non-Copy variable
  moves, and so do the non-Copy variables an alias-returning call leaf is
  given (moveck's `mvr-*` helpers).
- Bugs fixed: deftypes numbered in reverse; ADT ownership facts read only
  the first variant (accepted reuse of a moved value); merges re-added
  shadowed history; branch free of a reused `~aN` temp name (double free);
  freeing a static vec-literal loop seed.

Tooling:
- `tools/kernel_gate.sh`: fixpoint + fixtures + ASAN run of k2 compiling the
  kernel (`CATO_GATE_NO_ASAN=1` skips; `CATO_GATE_TMP` private scratch).
- `tools/alloc_site.c` (`ALLOC_SITE_ROOTS=1`, `-DPTAB_BITS=28` for large
  heaps), `tools/leak_site.sh`, `tools/asan_run.sh`, `tools/mem_bench.sh`.
- Kernel parity tests (`kernel_hir`, `kernel_emit`) run a native driver
  (seconds instead of ~15 min) and print line diffs.
- New HIR parity fixtures: `arm_frees`, `branch_frees`, `loop_drops`.

## Remaining steps

1. **Profile k2's remaining gap** (167 vs 115 MiB): `alloc_site` roots on
   k2 (see "How to verify"); largest kernel-only leaks were string buffers
   in `emit/` and `read/scan`.
2. **Port the rest of the seed planner to the kernel**:
   - `div_moved` / `always_recurs` in if/match merges (precision; kernel is
     conservative today);
   - escape transfer (`CATO_NO_ESCTRANSFER` rule) and merge ownership
     (`transfer_merge_tails`);
   - parameter-mode inference (kernel parity currently runs the seed with
     inference off: `build_program_located_with(prog, false)`).
3. **Kernel performance**: k2 is ~0.6 s slower than k1 on the same source.
   `sample` showed `clone__TyEnv`/`drop__TyEnv` (type env copied per child
   in `bcheck`) and `cato_stab_find`/`strcmp` (linear symbol lookup).
4. **Cleanup**: drop the now-unused `BCtx` fields (`loopcopy`, `rok`,
   `lseed`) and their 32 destructuring sites; `moveck.cato` (~4.4k lines),
   `lower.cato` and `ownership.rs` exceed the 400-line guideline — split.
5. **Strings**: length-carrying `str` (low hotspot, measured).
6. **Stage 5**: `Ptr` raw pointers and an arena library.
7. Noted follow-ups: inline all-scalar ADTs as Copy; planner-level clone
   elision when other args only read; NULL-safe extern `str` in the kernel
   emitter; kernel emitter `str_bytes` 6× slowdown; the seed's backend
   decides in-place loop appends itself while the kernel emitter relies on
   the planner's bit (a literal-seeded self-update differs in the HIR dump).

## How to verify

```sh
cargo build -p cato-cli --release && cargo test --workspace     # 344 pass
tools/kernel_gate.sh            # FIXPOINT: OK, ASAN k2: OK, GATE: PASS
tools/mem_bench.sh /tmp/mb      # table above
# leak roots of k2 (frame records + interposer):
#   k1 compile selfhost/cato/cli.cato --emit-ir -o k2.ll, add
#   "frame-pointer"="all", link, run with
#   DYLD_INSERT_LIBRARIES=site.dylib ALLOC_SITE_OUT=out ALLOC_SITE_ROOTS=1
```
