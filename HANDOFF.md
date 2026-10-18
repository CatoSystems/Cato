# HANDOFF: Kernel Clone Elimination & Memory Churn Optimization

## Round 14 results (2026-10-18): the retention fix — peak live 2.93 GB → 0.17 GB

**First: the question.** The kernel self-build used ~18× the seed's memory
(1178 MiB vs 64 MiB RSS compiling cli.cato). Hypothesis: allocation
alignment. **Answer: no.** Alignment/quantum rounding is 15% in the kernel
and 18% in the seed; the real cause is **retention**: 85% of the kernel's
89M allocations were never freed (peak live 2927 MiB ≈ cumulative churn).
The tools that showed this are `/tmp/alloc_live.c` (peak-live + quantum
waste interposer) and `/tmp/dbl_free.c` (double-free catcher with stacks);
`/tmp/alloc_leak.c` + `/tmp/dbg_leak.c` attribute never-freed bytes to
call sites (with tools/instr_clones.py bracketing). Consider promoting
alloc_live into tools/.

**The planner rules that leaked (all fixed in the seed first).**

- **R1 — call-scrutinee temps were never freed.** `(match (f x) (R [a b] …))`
  leaked the result box and every unmoved field, every execution. 653 of
  the kernel's 1585 `match` forms are this shape. Fix: `HArm.exit_free`
  (planner output, `Some(skip)`/`None`) — the backend nulls the skipped
  binders' field slots at arm exit and calls the box's drop glue, which is
  now null-tolerant (a nulled slot is "not ours to free"). A named box
  frees at arm exit only when some arm transferred an alias out; otherwise
  it keeps its scope free and stays readable after the match.
- **R2 — moved-binder boxes leaked wholesale.** Moving one binder out of a
  matched box suppressed the box's scope free entirely, stranding every
  sibling field. Fixed by the arm-exit free (skip the moved field, free the
  rest).
- **R3 — path-insensitive moves.** A binding moved in one branch of an
  `if`/`match` and not another was consumed everywhere and leaked on every
  path that kept it (`find-variant-2`-shaped scans leaked per step —
  897 MiB of `clone__TyEnv` alone). Fix: **branch-local frees** — each
  non-moving branch frees the binding itself through the existing
  empty-binds `Let` wrapper (`branch_frees` in ownership.rs). Scrutinee
  vars are excluded (their arms' exit frees own them); a branch whose value
  may alias the binding (`tail_may_alias`, new any-path analysis) is left
  alone.
- **R4 — escaped binders did not transfer ownership.** `(let [x (match (f)
  (R [a b] a))] …)` left `a` unowned forever. The arm-exit free skips `a`'s
  slot and marks it Moved, so the receiving let owns (and frees) the merge.
- **R5 — fresh temps as arguments leaked.** `(f (str …))`, `(str … (g x))`,
  `(match (f (g x)) …)` — constructors, `str`, `conj` and `spit` copy or
  consume; nothing freed the temps (~450 MiB: `str-rep`, `sb-str`,
  `md-mint-str`, `em-ins`). Fix: **hoisting** — such arguments are rewritten
  into `(let [~aN <temp>] (f … ~aN …))`, and the ordinary let planning
  frees `~aN` after the call. Guarded by `compute_alias_ret` (functions
  whose string/collection result may be a parameter's buffer or
  runtime-owned memory — externs included — are never treated as fresh).
  Order is preserved: every effectful argument before a hoisted one is
  hoisted too, under its own type. Planner-made `let`s may wrap match
  scrutinees (names minted `~aN` cannot appear in source); the backends'
  `adt_of` looks through them.
- **R6 — loop results and call-returned vectors never freed.** A loop whose
  completing value is a fresh-seeded accumulator now yields owned duty
  (`tail_yields_owned`'s `Loop` arm), and `free_binding_storage` releases
  read-only `c.vecs` bindings the planner owns (L-c, unblocked by the
  alias-return guard). `sb-str` seeds its accumulator with `(split "" "")`
  — a fresh heap vector — so the final buffer is always owned and freed
  after the join (−325 MiB).
- **Kernel-source fixes** (in the WIP tree, not mine but found on the way):
  `b-recur-kids` passed `args` as both the owned list and the borrowed
  sibling-read source — one value in two parameter positions is an alias,
  and the sibling walk read already-consumed elements (ASAN-confirmed UAF;
  any layout perturbation crashed the self-build). The borrowed position
  now takes `(clone args)`. Also fixed two latent seed emitter bugs the
  hoisting exposed: `emit_let`'s map/set-from-call header fills went into
  the entry prelude (dominance violation) — allocas hoist, fills stay in
  the current block.

**Numbers** (seed compiling `selfhost/cato/cli.cato`, clang excluded):
peak live 2927 → 178 MiB; RSS 1178 → 201 MiB; never-freed 85% → 5.5%;
allocations 89M → 81M (hoisting removed redundant copies). Wall time for
the self-compile is unchanged (~9 s; the WIP recur-ok port set that
baseline); user CPU +~25% for the frees, sys time down (fewer page faults).
Byte-identity held through every planner step: k1's IR for cli.cato was
byte-identical to the pre-change k1 until the deliberate strbuf/str-rep
source rewrites. Gate: PASS; parity suite: PASS (it caught the prelude
bug); probes in /tmp/probe (a–f: call-scrutinee, let-bound, clone-to-param,
moved-binder, conditional-move, hoist) all at 183 never-freed = pre-main
allocations only.

**Remaining (the port):** `kernel_hir` now fails on exactly the hoisting
diff — the kernel's `moveck.cato` must mirror the seed planner: `RBTable`
needs return types; `fp-scrut-adt` needs HCall/HClone/HMatch/HIf/HLet;
then fresh-box/hoist (order-free, hoist-ty, fresh-temp-ty), the arm-exit
plan in `b-arms` (skip sets, freeable-alias threading, escaped transfer),
`branch_frees` for HIf and across arms, `allocates-fresh` with the
alias-return guard, and loop-result ownership in the let merge. The HIR
shape is already ported (`ArmFree`/`HACons` 5 fields, dump format
`(arm v [binds] body -|[skip])`, emitter `arm-exit-free` + null-tolerant
boxed glue + `adt-of` HLet) — only the decision logic is missing.
`kernel_bootstrap`/`kernel_emit` closures-fixture failures are pre-existing
(reproduce identically with the HEAD seed on this tree).


## Status
- Kernel self-build alloc churn: **50.56 GB → 3.00 GB**; ALLOC_CALLS
  **1.19B → 83M**; wall time 47.5 s → ~5 s. (Round 12 spent +179 MB of
  the churn budget on ownership soundness; Round 13 recovered 67 MB of
  it — see Round 13. Round 13 also found that the Round 12 "Gate: PASS"
  and "tests green" claims were false: every verification instrument had
  gone stale. See the stale-verification list and re-run everything
  honestly before trusting a baseline.)
- Gate: PASS (fixpoint k1==k2==k3, t1–t6, bk1/bk3 ok, bk2/bk4 rejected;
  the gate now cleans stale /tmp/run_* artifacts — see Round 13).
- Tests: `cargo test` fully green (all 26 suites, including
  `cato-interp kernel_emit` and `kernel_hir`), `tools/tests` 20/20.
- CPU: ~40% of the self-build is now the kernel *waiting for clang*
  (`__wait4_nocancel` samples); of the rest, allocation/free dominates
  (clone/drop glue), strlen is down from ~90% to ~4%.

## Round 13 results (3.065 → 2.998 GB; the watch-pair threading + honest verification)

**First: the tree this handoff inherited was NOT green, and the
instruments that said so were lying.** Each find below was reproduced
before its fix; all are fixed and everything is verified honestly now.

- **The gate faked a PASS with stale artifacts.** `kb()` verifies
  acceptance by artifact existence, and the old script never removed
  `/tmp/run_*` — binaries from an earlier k1 made `OK bk1` true while
  the current k1 rejected it. The script now cleans `/tmp/run_*`.
  Rule: after any checker change, `rm -f /tmp/run_*` before trusting
  any acceptance-based harness.
- **B13 — the Round 12 esc-transfer port dropped the seed's guard.**
  `move_var`'s any_alias_escape transfer in the seed is guarded by
  `!loop_copy.contains(sv) && !scrut_borrowed_param`; the kernel called
  `move-var` unconditionally, so the legit `&T` shape
  `(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))` died
  with "cannot move n: it is a borrowed parameter" — bk1/bk3 were
  REJECTED. Fixed in `bcheck`'s HMatch arm (guard mirrors the seed).
- **The S8-era driver arity bug.** `check-program` takes
  `(defs lams facts tab adts)`; `cli.cato` passes five args but
  `hir_ref.cato`/`emit_ref.cato` passed four — the interp zips params
  with args, leaving `adts` unbound, surfacing as
  `interp: unbound var adts (fn b-retb)` (the fn label is a red
  herring: INTERP_FN is never restored on return, so the panic names
  the last *called* function). The seed checker does not flag
  wrong-arity calls — that gap is what let this ship; it cost a
  segfault debugging cycle later (a misaligned 8-for-9-arg call to
  `bcheck` compiled cleanly and crashed at `ldr x0`). Fixed both
  drivers; the arity gap is recorded as seed-side work, unfixed.
- **Stale S5-era table typing in the test drivers.** The magic symbol
  table is an `i64` runtime handle everywhere (`scan`, `build-program`,
  `dump-forms`, cli.cato) — but `hir_ref.cato`/`emit_ref.cato` declared
  `tab :- (Vector str)` and passed the i64 result of `(magic-table)`
  into it. The seed's LLVM backend marshals a Call in a vec-typed
  position through `emit_vec_call`, which panics on the i64 signature
  (`call magic-table does not return a vector`). `cato run` (interp)
  never noticed — values are untyped. Drivers now say `tab :- i64`.
- **The S6 wire dump carried the param-free wrapper.** The spec's
  oracle is `build_program_located` = `plan_drops(false)`: let-level
  frees, in_place bits, capture_owns — but NOT the defn-level
  `(let [] [params…] body)` param-free wrapper (that is the
  `plan_drops(true)` compile-pipeline stage, run after opt). The
  kernel's single `build-program` always wraps, so `closures.cato`'s
  `call1 [f:(Fn 0)]` dumped `(let [] [f] …)` where the oracle had none.
  Fixed at the dump boundary: `d-unwrap-param-frees` in `dump.cato`
  sheds exactly that shape (HLet with no binds and a non-empty frees
  list — a user `(let [] body)` binds nothing so its frees are empty,
  making the shape unambiguous). Codegen keeps the wrapper.

**The watch-pair threading (the Round 12 recovery lever) landed.**
`watchsc`/`watchbs` are gone from `BCtx` (11 fields again);
`bcx-with-watch`/`bcx-without-watch` are deleted. The pair rides as
`wsc :- i64 wbs :- &Sids` beside `bcx` through the 13 checking
functions (`bcheck`, `b-kids`, `b-default`, `b-let-binds`, `b-loop-binds`,
`b-arms`, `b-caps`, `b-call-move{,-ps,-walk}`, `b-ctor-move`,
`move-var`, `move-if-nc`); `use-var` and the read-only helpers don't
need it. Semantics mirrored exactly:
- armed arm (boxed ∧ named scrutinee ∧ mentioned ∧ non-empty alias
  set) → the body walks with `(scrid, aliasing-binders …)`; otherwise
  the outer pair passes through — `(clone wbs)` costs one small-list
  clone per non-armed arm (usually `SNil`), vs the ten-table BCtx
  rebuild per armed arm before.
- a loop body still resets the watch (`-1 (SNil)`), matching the old
  `bcx-with-loopcopy` reset; the b-arms recursion passes the *outer*
  pair to the next arm.

Measured: **−67 MB, −1.37M alloc calls** (3.065 → 2.998 GB; 84.36M →
82.99M). That is the whole `bcx-with-watch` term of Round 12's +179 MB
— "the dominant term" was ~37% of it, not most; the rest is the
no_move/watch-scan machinery itself (the `b-mentions` pre-scan walk is
CPU, the watch lists are small). Decision-identity is proven, not
argued: the post-refactor k1 emits **byte-identical IR** for all eight
kernel fixtures and **identical rejection messages** for all four
error programs vs the pre-refactor k1; gate fixpoint OK; `cargo test`
and `tools/tests` fully green.

Seed-side gaps found on the way (documented, not fixed):
- The checker does not reject wrong-arity calls (see above; an E-code
  for call arity would have saved two debugging cycles today).
- The interp's `unbound var (fn …)` label names the last-entered
  function, not the panicking one (INTERP_FN is never popped).

## Round 12 results (ownership soundness + selfhost checker parity; 2.88 → 3.06 GB)
An audit of the ownership model found confirmed memory-unsafety bugs in
both compilers. All are fixed; every fix has a probe (kept in
/tmp/cato_audit, with an ASAN/footprint harness — LeakSanitizer is NOT
available on this macOS, `cato build --asan` compiles the .ll at -O2
which optimizes the toy double-frees away; the working recipe is
`clang -O0 -fsanitize=address <(cato --emit-ir [--no-opt]) runtime.c`).

Seed-side confirmed bugs (each reproduced before the fix):
- **B1 — moves inside `for`/fused-loop bodies were never checked**
  (`ownership.rs` skipped the nodes entirely). The same double move that
  E301s when written flat compiled inside a `for` body and double-freed
  per iteration (ASAN-confirmed, optimizer-independent). Now the body is
  checked against the live state and moves of pre-loop bindings are
  rejected (`forbid_pred_moves`; per-iteration bindings may still move).
- **B2 — copy-prop dissolved single-use owning ADT/Str/Fn bindings**
  (`simplify.rs` guarded only Vec/Map/Set): the binding vanished, its
  free with it, and the box leaked. Probe: 242 MB → 1 MB at 5M
  iterations.
- **B4 — `count_expr_is_vec` missed Loop/For/Conj/Subvec/Clone**:
  `(count (loop ...))` emitted `strlen` on a `%VecI64` (clang-rejected
  IR). Now type-directed arms; m1 probe compiles and runs.
- **B5 — `for` as a loop-binding seed** panicked
  (`funcs.rs` `emit_vec_pair` had no For arm); now emitted.
- **B6 — `(match (let ...) ...)`: checker accepted, backend panicked** —
  and the panic was protecting a double-free (the scrutinee let's free
  runs inside `emit_adt` before the arms). Now rejected: "bind it to a
  name first"; `adt_of` gained safe Seq/Loop arms.
- **B7 — vector literals inside loops emitted per-iteration `alloca`**
  (`materialize_vec_lit` / `vec_arg`'s VecLit arm): 50k iterations fine,
  5M → SIGSEGV. Allocas hoisted to the entry prelude (the kernel emitter
  already hoisted via `em-pre`).
- **B8 (new find) — a non-`&T`-marked function returning a borrowed
  param** (`(defn f [b :- &Box] :- Box b)`) compiled and the caller
  freed the alias → ASAN double-free. Both checkers now reject it via
  the inverse of `tail_borrow_ok`.

Leak classes fixed (footprint deltas at 1–5M iterations vs controls):
- **L-a** if/match owning merges: arm-tail duty transfer at bind sites
  (`transfer_merge_tails`; arms that only alias keep the old behavior).
  97 MB probe → 1 MB.
- **L-b** `(Vector str)` scope frees release only the pointer array:
  now `cato_str_vec_free` in `free_binding_storage` (seed) and
  `fallthrough-free` (kernel emitter) — the ADT glue already did this.
- **L-f** loop accumulators: `owns` seeded for loop binds (with owns
  restore — a stale owns=1 past the loop would let a later conj fire
  in-place on an aliased buffer), plus the recur self-update rule: the
  planner arms `recur_ok` for exactly the backend's `recur_fast` shape
  (single conj of a fresh-seeded loop var, no sibling recur argument
  reads it). The backend frees grown-out buffers on the planner's
  in_place verdict. 150 MB → 68 MB (the rest is the c.vecs
  final-buffer class).
- **L-g** scope-aware shadowing: the defn-wide "bound twice, never
  frees" pre-pass became a per-let `rebound_in_body` walk
  (`b-rebound` in the kernel) — sibling-branch lets of the same name
  are distinct bindings and each frees its own buffer. 9.3 MB → 1 MB.

Selfhost checker parity (the audit's B3): `moveck.cato` was a weakened
fork — the old k1 ACCEPTED `examples/errors/move_borrowed.cato` and a
borrowed-param shape (d4) whose binary ASAN-confirmably double-freed.
Ported: `no_move` + borrowed-param error (seed-identical message),
watch lists (scrutinee moved inside its own arm retires aliasing
binders; armed only for a named owned scrutinee the arm body mentions —
temps are never freed, so watching them is pure churn), loop-init
moves, `loopcopy` guards (use-var, let-of-var, accumulating nested
loops), B8, borrowed-result-into-owned-param, loop-bind owns seeding
with restore, scope-aware shadowing. D1/D4 now reject in k1 with the
seed's exact message. Remaining divergence (documented): the watch is
innermost-only and `fp-scrut-adt` is still HVar/HCtor (call-scrutinee
binders are leaky-not-unsound because the temp box is never freed).

**The trap this round** (cost a gate failure): the first free verdict
keyed off the backend's `recur_fast` alone. `sb-str` seeds its
accumulator with the `[]` LITERAL — static global — and the first
growth freed read-only data ("pointer being freed was not allocated",
k1 abort). Rule: **a free keys only off the planner's in_place verdict
(owns-gated); `self_update` may append in place but never free.** The
`@`-prefix guard in emit_conj stays as a belt but cannot see loaded
pointers — it is not a substitute. Also: rebuild BOTH seed binaries
before the gate (the gate uses target/debug/cato; a stale debug seed
emits different IR and wastes a debugging cycle).

Churn accounting: +179 MB total. `bcx-with-watch` clones program-wide
tables (dparams/retb/adts/shared) per armed arm — the dominant term.
Recovery lever if it must go: thread a small `(wsc, wbs)` pair beside
`bcx` through bcheck instead of rebuilding BCtx (~30 helper
signatures).

## Round 11 results (no gain; the `TyEnv` conversion spec)
Checked the other two "narrow read" candidates and rejected both on the same
grounds as round 10: `vlist-at`'s three remaining callers (`find-variant-2`,
`variant-fields` at 2574, `fill-variants`) need the variant's *name and* its
field list, so two narrow walks cost more than the row copy they save. The
remaining cheap conversions are gone.

**The next real lever is threading the type environment back through `LRes`**
(the trick that worked for `BCtx` in round 9), worth ~0.58 GB:
`(deftype LRes (LOk [tab adts insts sigs lams rets ncached defs e]) ...)` —
adding `env :- TyEnv` and constructing every `LOk` with the environment the
function *received* lets each caller reuse it for its next sub-expression
instead of copying it. Sizing: **59 `(LOk …)` constructions and 58 `LOk [`
matches** in `lower.cato`.
- The trap: an arm that *extends* the environment for a let body
  (`(lower body (te-put env k v) …)`) must still construct `LOk` with the
  **original** `env`, otherwise the extension leaks out of the let and the
  caller lowers the rest of the enclosing expression with the wrong scope.
  Audit each of the 59 sites for which `env`-named variable it has in hand.
- Matches are mechanical (append a binder to the `LOk [` vector); the
  constructions are the ones that need the audit.
- Guard the edit the way the round-9 mistake was caught: after any structural
  edit, diff this file's top-level definition names against `HEAD` before
  compiling, and close restructured forms by counting depth rather than
  eyeballing parens.

## Round 10 results (no gain; how to test a narrow-read conversion)
Tried the `adts-at` narrow-read conversion on the hottest caller
(`subst-ty`'s ADT arm): added `adts-tpl-at` / `adts-tplargs-at` and split the
arm into a helper so it stops materialising the whole `AdtInfo`. Measured:
2,877,450,668 -> 2,881,993,910 B (**worse** by 4.5 MB, +110k calls), because
the original `(let [info (adts-at ...)] (match info (AdtInfo [...] ...)))`
consumes the copy immediately — the optimizer elides it — while the narrow
version walks the table twice (once per field) and still copies the field
list. Reverted; the tree stays at 2.877 GB.

**Rule of thumb for the remaining `*at` sites:** a whole-row clone is only
worth replacing when the row survives in the caller (is bound and then used
through several branches, or stored). If it is destructured and dropped right
away, the copy is already free and a narrow read adds a second walk. Check
with the inclusive attribution before and after, not by inspection.

## Round 9 results (3.27 → 2.88 GB): the context threading landed
The `check-defns` opening from round 8 is closed. `b-plan` now builds the
`BCtx` once from the program-wide tables, uses it, and returns it inside a
new `BPlanRes`; `check-defns` / `check-lams` thread that context into the
next definition instead of copying it for every call. Measured: 0.28 GB of
`DParams` (one entry per definition, cloned twice per definition) plus the
`AdtTab`/`Facts`/`Sids`/`RBTable` copies that travelled with it.

Details that mattered:
- Only the *per-definition* fields (`escaped`, `shadowed`, `loopcopy`) are
  rebuilt per definition; the program-wide ones move through untouched.
- `fp-frees` reads the maps and facts but took them by value, so it now
  borrows (`st :- &IMap owns :- &IMap facts :- &Facts esc :- &Sids`), and
  `fp-frees-of` reads both facts and `escaped` out of the context.
- Watch the evaluation order when a value is *moved into a result* and
  *borrowed in the same expression*: `(BPlanRes bcx2 (BPlan ... (fp-frees-of
  ... bcx2)))` moves `bcx2` first and fails. Read through the context into a
  `let` before handing it back.
- The same trap in reverse in `b-plan`: the precondition helpers borrow
  `facts`/`dparams`/`retb`, so their branches may then move those values into
  the context they return — one `BCtx` per branch, alternatives included.

## Round 8 results (3.31 → 3.27 GB) and the `check-defns` opening
- `b-facts-of` / `b-retb-of` hand back an *owned* field, so every read copied
  the ownership list (one cons per ADT) although the consumers
  (`b-allocates`, `facts-owns-at`, `b-arm-state`, `b-arm-esc`) only borrow.
  The reads are fused (`b-facts-owns-at`, `b-allocates-of`,
  `b-arm-state-of`, `b-arm-esc-of`). Same medicine as `md-*`/`ps-*`.
- **Concrete opening for the next round** (measured): in `check-defns` the
  program-wide tables — `facts`, `tab`, `shared`, `adts`, `dparams`,
  `retb` — are passed *unchanged* through the recursion
  (`(check-defns t (defns-append acc h) facts tab shared adts dparams retb)`)
  and only `ds`/`acc` vary. Yet each iteration clones all of them for the
  `b-plan` call (0.28 GB `DParams` + 0.07 GB `AdtTab` + the rest), because
  `b-plan-body` builds a `BCtx` whose fields *hold* them. Splitting the
  program-wide half into its own context, built once and passed by borrow
  (`&PCtx`) with a per-defn `BCtx` on top, removes that whole cluster. The
  per-defn fields of `BCtx` (`escaped`, `shadowed`, `lc`) are the reason it
  cannot simply be hoisted as-is.

## Round 7 results (no measured gain; where the floor is)
Two automation attempts were built, run and removed. Both findings matter:

1. **A callee-directed borrow conversion cannot be driven by the checker's
   error text alone.** `tools/borrow_params.py` (written, run, removed) read
   each "cannot move v in F" error, found the calls in F passing `v` to an
   owned `T` parameter, and flipped those callees; when a function *stores*
   its parameter (`lok` into an `LRes`, the `BCtx` builder) it was put back
   and pinned. It still cycled to the 400-round cap: the pinned set stops
   *one* loop, but the checker's per-message view has no fixpoint, so A
   flips B, B flips C, C flips A back with a tidier message. Doing this
   properly needs the call graph and a fixpoint over the *set* of borrowed
   parameters, not one error at a time. The half-built tool is deleted
   rather than left as a trap.
2. **Most of what is left is inherent to the data model, not over-cloning.**
   Sampling the remaining sites one by one:
   - `raws-find-2` clones a `RawDefn` (the whole body AST) because it keeps
     the last-declared match while walking a *borrowed* `Raws` chain — a
     borrowed struct cannot hand out an owned field.
   - `adts-at` / `vlist-at` hand back a whole `AdtInfo` / `Varnt` for callers
     that then use two or three of its fields (tpl + tplargs, name +
     fields); a narrow reader saves nothing when the fields are themselves
     owned lists that must be copied out.
   - `subst-ty`'s `(clone adts)` guards a table that a later alternative
     path still needs.
   So the next real step is a *representation* change, not more borrows:
   either a per-program shared context (see the `BCtx` note in round 6) or a
   way to return a borrowed field (lifetimes), which the language lacks.
   The cheap churn is gone; what remains is ~3.3 GB of field copies and
   per-definition context construction.

## Round 6 results (3.39 → 3.31 GB) and the structural blocker
- `build-raws` copied the parser state into `parse-defn` in both raw-form
  branches to keep a copy the branch never reads again; the callee threads
  it back through `RawsRes`.
- **Why the remaining clusters resist signature changes** (measured, so the
  next agent does not repeat the experiments):
  - `clone__TyEnv` (0.69 GB across `lower-ctor-fields`, `match-variant-arm`,
    `lower-str-args`, `lower-match-plain`, `lower-if`): the lowerer only
    *reads* its type environment, so borrowing the parameter is right — but
    the automatic blanket flip does not converge. The checker reports the
    *function that hands the borrow on*, never the callee that still wants
    ownership, and reverting the reporter cascades everything back to owned
    (tried twice: 135 flipped → 0 kept). A correct conversion must flip the
    *callee* named at each call site, which needs the call graph, not the
    error text alone (`lok` shows the other end of the trap: it *stores* its
    parameter into an `LRes`, so it genuinely needs ownership).
  - `clone__DParams` / `clone__AdtTab` / `clone__Facts` / `clone__Sids` from
    `check-defns` (0.4 GB): `b-plan-body` builds a `BCtx` whose fields *hold*
    that state (`(BCtx (clone escaped) shadowed (clone facts) (clone tab)
    shared (SNil) adts dparams retb)`), so the per-definition copies are
    structural. The fix is to build the shared context once per program (or
    make `BCtx` hold it behind a shared handle) rather than per definition.
  - `clone__Varnt <- vlist_at` (0.27 GB) and `clone__AdtInfo <- adts_at`
    (0.14 GB): both hand back a whole variant/row so the caller can read one
    field. Narrow readers help only where the caller does not need the field
    list itself.
- A half-built helper for the callee-directed flip was written and removed
  (`borrow_params.py`): it oscillates without call-graph reasoning. If you
  rebuild it, resolve the callee per call site and refuse to flip functions
  that store the parameter (constructors, `lok`, `BCtx` builders).

## Round 5 results (4.09 → 3.39 GB)
- **Over-cloning in branch alternatives.** `cond`/`match` branches are
  alternative paths, so each may *move* the parser state; the clones came
  from the original autoclone pass, which is conservative. The hot ones were
  `parse-ty`, `parse-ty-list` and 31 arms of `parse-ty-sym-str` — code that
  runs for every type annotation. (4.09 → 3.61 GB)
- **`build-lower-2` cloned its state per definition** (parser state,
  definition chain, instance list) in each of three branches, and the call
  into `lower-body-raw` copied the tables the recursive call immediately
  replaced with what lowering returned. (3.61 → 3.39 GB)
- Watch out for the move-and-borrow shape: `(RawsRes ps2 (RNil)
  (ps-err-msg ps2))` moves `ps2` into the constructor *and* borrows it for
  the message; read the message into a `let` first. Also remember that
  accessing a matched struct's fields inside its own match arm aliases the
  consumed scrutinee, so moving the scrutinee there invalidates them
  (`(RawRes ps ...)` inside `(match ps (PS [...] ...))` fails).

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
./target/release/cato selfhost/cato/cli.cato --emit-ir -o /tmp/k.ll
python3 tools/instr_clones.py /tmp/k.ll /tmp/i.ll /tmp/map.tsv
clang -dynamiclib -O2 -o /tmp/dbg_hit.dylib tools/dbg_hit.c
clang -O2 -Wl,-undefined,dynamic_lookup /tmp/i.ll runtime/runtime.c -o /tmp/ki
DYLD_INSERT_LIBRARIES=/tmp/dbg_hit.dylib DBG_MAP=/tmp/map.tsv \
  /tmp/ki build selfhost/cato/cli.cato /tmp/k3 2>/tmp/sites.txt
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
3.00 GB the profile has shifted again (Round 12 added the +179 MB
watch/no-move port; Round 13 recovered 67 MB of it by threading the
watch pair — see both churn accountings). Last measured leaders:
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
- Cato-level leak guards remain unexplored (compile-time rejection of
  the leak classes: control-flow merges of owning values bound to
  intermediates, call-returned vectors bound read-only).
- L-c is now unblocked: call-returned vectors/`split` results bind
  read-only (c.vecs) and never free. Materializing them into grow slots
  is sound only BECAUSE B8 now forbids returning borrowed collections
  from unmarked functions — do both together or neither.
- Goal: approach seed behaviour/resource profile (Round 12 spent
  +179 MB of it on soundness; Round 13's watch-pair threading recovered
  67 MB — the `bcx-with-watch` term).

## Files to focus
- `selfhost/cato/hir/moveck.cato` — the ported checker (no_move, watch
  pair `wsc`/`wbs` threaded beside `bcx` through the 13 checking
  functions, loopcopy, B8, per-let shadowing, B13 guard); the largest
  Round 13 edit site
- `crates/cato-core/src/hir/ownership.rs` — the reference planner
  (forbid_pred_moves, recur_ok, transfer_merge_tails, rebound_in_body,
  B8); change the seed FIRST, then port to moveck.cato
- `crates/cato-core/src/opt/simplify.rs` — the heap-owning Keep guard
  (B2); the guard list must match `heap_owning`
- `crates/cato-backend-llvm/src/llvm/heap_vec.rs` (free verdicts —
  planner-in_place only, never `recur_fast`), `adt.rs` (glue +
  `adt_of`), `exprs.rs` (`count_expr_is_vec`), `exprs_val.rs`/`funcs.rs`
  (hoisted allocas, For arm)
- `selfhost/cato/emit/adt.cato` (`fallthrough-free` str-vec release),
  `emit/heap.cato` (conj-grow static guard), `emit/*.cato`,
  `hir/lower.cato` (hotspots), `hir/dump.cato`
  (`d-unwrap-param-frees` — the S6 wire dump sheds the defn-level
  param-free wrapper; codegen keeps it)
- `selfhost/cato/hir/hir_ref.cato`, `emit/emit_ref.cato` — the S6/S7
  test drivers (now arity- and typing-correct: `check-program` takes
  five args, the magic table is `i64`)
- `tools/instr_clones.py`, `tools/alloc_stack.c`, `tools/alloc_profile.c`
  (attribution); `tools/kernel_gate.sh` (cleans /tmp/run_*); the Round
  12 probes/harness live in `/tmp/cato_audit` (ephemeral — re-create
  from the Round 12 notes if gone)

## Quick commands
```bash
./tools/kernel_gate.sh | tail -14
DYLD_INSERT_LIBRARIES=/tmp/alloc_count.dylib /tmp/k1 build selfhost/cato/cli.cato /tmp/k2 2>&1 | grep ALLOC
# exact per-call-site clone counts:
./target/release/cato selfhost/cato/cli.cato --emit-ir -o /tmp/k.ll
python3 tools/instr_clones.py /tmp/k.ll /tmp/i.ll /tmp/map.tsv
clang -O2 -c -o /tmp/dbg_hit.o tools/dbg_hit.c
clang -O2 /tmp/i.ll runtime/runtime.c /tmp/dbg_hit.o -o /tmp/ki
DBG_MAP=/tmp/map.tsv /tmp/ki build selfhost/cato/cli.cato /tmp/k3 2>/tmp/hits.txt
python3 -m unittest discover -s tools/tests && cargo test
cargo build && cargo build --release   # the gate uses target/debug/cato: rebuild BOTH
# (stale target/debug/cato emits different IR than release — a wasted
#  debugging cycle looks like a nondeterministic gate failure)
```

## Verification discipline that worked
For a kernel-source change: seed compile-check → build k1 with the seed →
`cmp` the IR that the **pre-change compiler** and the **post-change
compiler** emit for the *same* input (byte-identical ⇒ semantics-preserving)
→ gate. For an emitter change: gate fixpoint + fixtures + `cargo test`
(parity suites) + a focused micro-benchmark with identical output.

## Next agent todo
1. Re-run the inclusive attribution (the profile shifted again: Round 13
   removed the per-armed-arm BCtx rebuild). The known next candidates:
2. Fuse `dparams-get` with its two consumers (`b-call-move-ps`,
   `b-tail-args-ok`) so the param list stays borrowed; convert the
   remaining `(match st (Emit [c m s] (clone m)))` reads to `&Emit`
   accessors (targets `clone__StrCell` + `clone__PList`).
3. Chase the nested `clone__TyList` volume: find which remaining callers
   clone a whole `AdtTab`/`Sigs`/`AdtInfo` (entry sites are few — the
   instrumentation in the quick commands lists them), and shrink those.
4. L-c: materialize call-returned vectors/`split` results into grow
   slots (B8, now in place, is the precondition); also fixes the
   sb-str/loop-result final-buffer class.
5. Compile-time leak guards: the checker already knows L-a/L-c shapes —
   a `-Wleak` diagnostic tier (warn, don't reject) is cheap now.
6. Seed-side, low priority but real: an E-code for wrong-arity calls
   (two debugging cycles today were caused by its absence — the interp
   shows a bogus unbound-var panic instead); and the interp's
   `INTERP_FN` label should be saved/restored around calls.
7. Keep changes semantics-preserving; run the gate after each batch.
   Remember: rebuild BOTH seed binaries first, never key a free off
   `recur_fast`, and `rm -f /tmp/run_*` before trusting an
   acceptance-based harness.
