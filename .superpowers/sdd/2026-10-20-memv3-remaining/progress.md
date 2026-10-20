# SDD ledger — plan: PROGRESS.md (memory model v3, "Remaining steps")

Spec: docs/superpowers/specs/2026-10-19-cato-memory-model-v3.md (Part V).
Execution: inline on branch `mem-v3-next`.
Baseline: 30d1f68. Gate PASS: FIXPOINT OK, ASAN k2 OK, fixture suite OK;
ALLOC_BYTES 3.867 GB / 96.88M calls. mem_bench k1 116 MiB / k2 168 MiB peak.

## Step 1: profile k2's remaining gap — DONE

Root-leak attribution (k2's own planner+emitter, `tools/alloc_site.c`
ALLOC_SITE_ROOTS=1, frame-pointer build of k2.ll):
  #1 strbuf__sb_str <- emit__fn_text          14.8 MB (12,125 roots)
  #2 clone__Asts   <- lower__lower_call_args  13.4 MB
  #4 lower__lok     <- lower__lower_node      10.7 MB
  #5 clone__Asts   <- forms__parse_defn_body   9.0 MB

Ruling: step 1's "encode each function's own copy semantics in the
exports table" is rejected on measured evidence — `clone__TyEnv <- bcheck`
(the item that suggested it) does not appear in the root report at all
(bcheck borrows the env since commit 38bc156). The real kernel-only class
is the `strcat` argument-temp hoist. Cost if wrong: the export-table idea
stays unproven; no code changed for it.

Finding (pre-existing kernel/seed planner divergence): the kernel's
`hz-order-free`, `hz-fresh`, `hz-hoist-ty` lack the seed's Clone/If/Match/
Loop/Get/StrNth/ParseNum/Map/Set/Subvec arms (`ownership.rs:2226-2311`).
Consequence: a `(str a a a)`-style arg list never hoists in the kernel,
and the kernel's `hz-plan` even *stops* at the first arg it cannot type
(`hz-none ht` -> `acc`) where the seed `break`s the whole plan.
Probe: `(defn big2 [a :- str] (str (str a) (str a) (str a) (str a)))`
  seed:   (let [~a0 (strcat a) ~a1 (strcat a) ~a2 … ~a3 …] (strcat ~a0 ~a1 ~a2 ~a3) [~a0 … ~a3])
  kernel: (strcat a a a a)     <- 4 fresh buffers/execution leak

## Step 2a: port the missing hoist precision from the seed

Plan: bring `hz-fresh` / `hz-hoist-ty` / `hz-order-free` to seed parity
(mirroring the seed's arms), plus the `hz-plan` break-on-untypeable-arg
stop. Change the seed's hot twin wherever the kernel is found wrong
(`fc.md-mint-str`), then port.

Task 2a.1: hz-fresh Clone arm.
  seed `fresh_temp_ty`: `Clone(inner)` -> `Var(v)` → `tys[v]` filtered by
  `want`; otherwise recurse into `inner`. Kernel: same shape present.
  Verified equal.

Task 2a.2: hz-fresh / hz-hoist-ty missing arms + hz-plan break.
  Verified RED: kernel `noisy`/`mix`/`big2` fixtures drop Clone/ToStr args.
  Expect after GREEN: kernel dump == seed dump.

Task 2a (hoist parity): DONE
- Kernel hoist port: hz-plan-step/hz-step-plan (stop on untypeable arg, seed break semantics), hz-fresh Clone/If/Match/Loop/Get/StrNth/ParseNum arms, hz-hoist-ty additions, removed premature single-arg str fold in lower.cato.
- Parity: kernel_hir + kernel_emit PASS; GATE PASS (fixpoint, ASAN k2, all fixtures).
- Leak fix verified: (str a) passed to owned param now gets a freed copy in kernel-built IR (matches seed).
