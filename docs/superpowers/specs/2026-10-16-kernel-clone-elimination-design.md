# Kernel Clone Elimination — Design

Date: 2026-10-16
Status: **Proposed.** Implementation plan to follow via `writing-plans`.

## 1. Summary

The self-hosting kernel compiles itself at a peak of ~8 GB RSS; the Rust seed
compiles the same source in **0.22 GB**. The two produce **byte-identical**
LLVM (`k1.ll == k2.ll == k3.ll`), so the gap is not codegen — it is the
*runtime memory behaviour* of the generated kernel. Instrumenting `malloc`
shows ~3.0 billion allocations totalling ~116 GB churned to reach that peak,
overwhelmingly inside `clone__*` functions.

The cause is structural and now precisely located: the kernel source is a port
of a Rust compiler that used borrowed references (`&T`, `HashMap` behind `&`),
but the Cato source stands in for those references by writing `(clone X)`
everywhere. These clones are **not required by the ownership model** — the
compiler accepts the same code with the clones replaced by moves (verified;
§3). Removing the redundant clones is therefore a large, mechanical,
compiler-checked optimization.

This spec defines the clone-elimination work: the primary lever (delete
redundant `(clone …)`), a secondary lever (borrow table/IR consumers via
`&T`), and the measurement methodology. It supersedes the "de-functionalize
the tables" framing of `2026-10-16-borrowed-returns-design.md` §5: the tables
matter, but the dominant cost is cloning IR trees on the rebuild path, and it
can be attacked without changing any data structure.

## 2. Evidence (measured)

| compiler | peak RSS to compile `selfhost/cato.cato` |
|---|---|
| seed (Rust) `target/debug/cato` | **217 MB** |
| kernel | **~8.1 GB** (was ~9.0 GB at HEAD before `2aaa847`) |

Fixpoint is preserved byte-for-byte across the bootstrap
(`k1.ll == k2.ll == k3.ll`), confirming the kernel's codegen equals the seed's;
the difference is only how the generated program uses memory at runtime.

**Clone profile** (malloc interposition on the current kernel; ~3.0 B calls,
~116 GB total). Bytes are from the sampled-backtrace run and call counts from
the earlier exact-count run of the same kernel; both are cumulative churn, not
peak:

| clone site | volume | calls |
|---|---|---|
| `clone__HExpr` | 47.7 GB | ~612 M |
| `clone__TyList` | 25.2 GB | ~630 M |
| `clone__HExprs` | 9.9 GB | ~421 M |
| `clone__VList` | 9.2 GB | ~231 M |
| `clone__Sids` | 8.6 GB | ~353 M |
| `clone__AdtTab` | 5.7 GB | ~80 M |
| `clone__PList` | 4.6 GB | ~75 M |
| `clone__HArms` | 3.3 GB | ~80 M |
| `clone__TyEnv` | 1.8 GB | ~35 M |
| `clone__Defns` | 1.6 GB | ~18 M |
| `clone__StrCell` | 1.5 GB | ~61 M |
| `clone__Binds` | 1.3 GB | ~24 M |

Two buckets: **IR trees** (`HExpr`/`TyList`/`HExprs`/`HArms`/`Binds`) ≈ 88 GB,
and **tables** (`VList`/`Sids`/`AdtTab`/`PList`/`TyEnv`/`Defns`) ≈ 31 GB.
`TyList` is dominated by `Ty` fields embedded in `HExpr` nodes, so it is
largely downstream of `clone__HExpr`.

**Ruled out:**

- The builtin `Map`/`Set` (refcounted HAMT runtime, O(1) clone) is unusable
  for these tables: codegen forbids them as function parameters/returns
  (`crates/cato-backend-llvm/src/llvm/funcs.rs:266,308` — "maps are local
  values").
- Missing clone-elision at codegen: `k1.ll == k2.ll` proves the kernel's
  output already equals the seed's, which runs the seed's `opt::clone_elide`
  pass. There is nothing left to elide at that level; the waste is in the
  source's explicit clones.

## 3. Root cause

Kernel functions are written as tag-dispatch + projecting accessors and clone
their way through the IR, e.g. `hir/moveck.cato` `bcheck`:

```cato
(defn bcheck [e :- HExpr st :- IMap ...] :- BRes
  (match e
    (HBinW [op a b ty]
      (match (bcheck (clone a) st owns rem cons (clone tys) bcx)   ; clone child
        ...
        (BOk st2 owns2 rem2 cons2 (HBinW op c1 c2 ty)))))
    (HConstI64 [v] (BOk st owns rem cons (clone e)))))             ; clone node
```

`e` is a **boxed** ADT parameter. `match` binds `a`, `b`, `ty` as field
aliases. The author cloned them into the recursive call and cloned `e` to
return it — mirroring Rust's `.clone()` on `&HExpr`.

**The clones are not forced.** The ownership model permits moving match
binders over a boxed scrutinee into owned parameter positions, and permits
returning the scrutinee directly. Verified with the seed (the acceptance
oracle) on a minimal mirror of `bcheck`:

```cato
(deftype Ty (I [x :- i64]))
(deftype T (C [a :- T b :- T ty :- Ty]) (L [v :- i64]))

(defn f [e :- T op :- i64] :- T
  (match e
    (C [a b ty]
      (match (f a op)                        ; a moved, no clone
        (C [a2 b2 ty2] (C a2 (f b op) ty2))  ; b, ty2 moved
        (L [v] (L v))))
    (L [v] (L v))))

(defn g [e :- T] :- T
  (match e
    (C [a b ty] e)                            ; return e, no clone
    (L [v] e)))
```

Both functions build and pass the seed's ownership checker
(`target/debug/cato build mtest2.cato` → `✓ built`). The seed's `clone_elide`
pass never removes them because its eligibility is narrow (let-bound
single-use / static literals), not match binders.

So the optimization is: **replace `(clone X)` with `X` wherever `X` can be
moved**, letting the ownership checker reject anything unsafe. This is
mechanical, reviewable, and cannot silently miscompile an accepted program.

## 4. Design

Three levers, applied in order, each separately committed and measured.

### Lever A — eliminate redundant clones (primary)

For each `(clone X)`:
- If `X` is a local (match binder, let-bound name, or parameter) and this is
  its **only remaining use**, replace `(clone X)` with `X` (a move).
- If `X` is used again later, keep exactly one clone (or, when the later use
  is a borrow context, restructure so the value is moved once and borrowed
  elsewhere).
- Never remove a clone whose source is later moved/freed without a
  replacement owner — the checker catches this as a use-after-move error.

Applied per file, in order of clone volume: `hir/moveck.cato` (rebuild walks),
`hir/lower.cato` (typing/substitution/instantiation + node construction),
`emit/*`. Note `b-count-vars`/`b-count-binds` were already converted to
borrowing `match` walks in commit `2aaa847`; the same treatment extends to
`lower.cato`'s helpers.

The rebuild passes (`bcheck`, `lower`, substitution) already *intend* to
consume their input and produce a rewritten tree sharing unchanged subtrees;
deleting the clones realizes that intent. Where a subtree is genuinely needed
twice, the checker forces a clone, which is correct.

### Lever B — borrow the consumers (`&T`)

Where lever A cannot help because a callee takes a table/IR parameter by value
and only reads it, convert the parameter to `&T` (the feature from
`2026-10-16-borrowed-returns-design.md`), drop the matching `(clone X)`
argument at call sites, and stop threading tables through the results of
*pure readers*.

Targets are the remaining table clones: `adts` (VList/AdtTab), `env`
(TyEnv→Sigs→TyList), `sigs`, `insts`, `defs`, `seen` (Sids). The key rule: a
parameter converted to `&T` **must** have its `(clone X)` arguments dropped,
or the clone leaks and RSS regresses.

### Lever C — cheap table extension (only if measured to matter)

`adts-append-info` and `adts-set` rebuild the whole cons-list (O(n) per
append → O(n²) table construction). If per-phase measurement shows table
construction dominates, switch to prepend-and-reverse or an accumulator
threaded once. Deferred until the spike identifies it.

## 5. Methodology (measure first)

1. **Spike (plan task 1):** instrument per-phase peak RSS in the kernel
   (parse / expand / lower+mono / moveck / emit) and apply lever A to one hot
   function (`bcheck`). Rebuild via the seed and measure. This validates that
   clone churn drives peak RSS before committing to the full rewrite, and
   produces the before/after numbers the rest of the plan targets.
2. **Per-file lever A** with the full gate after every step (below).
3. **Lever B** only for what lever A leaves.
4. Record peak RSS and the top-`clone__*` profile at each step.

### Gate (every step)

```bash
python3 tools/concat_kernel.py
./target/debug/cato build selfhost/cato.cato -o /tmp/k1
/tmp/k1 build selfhost/cato.cato /tmp/k2      # kernel self-build
/tmp/k2 build selfhost/cato.cato /tmp/k3
cmp /tmp/k2.ll /tmp/k3.ll                      # fixpoint: must be identical
# t1..t6 outputs: la=X, X, 7, 3, 3, 3
# seed/kernel agreement: bk1 accept, bk2 reject E304, bk3 accept, bk4 reject
/usr/bin/time -l /tmp/k2 build selfhost/cato.cato /tmp/mem   # peak RSS
```

Correctness for this work is defined by the fixpoint + the kernel test
fixtures, because LLVM-emitter tests do not cover most kernel functions.
Additional safety net:
- The ownership checker (seed when building `k1`; the kernel's own moveck when
  the kernel compiles itself) rejects every illegal move — so an accepted
  clone removal is safe by construction.
- ASAN on the self-compile (`clang -O0 -g -fsanitize=address`) with no
  over-free/UAF.
- `cargo test -p cato-core` stays green.

## 6. Acceptance

- Fixpoint `k2.ll == k3.ll` byte-identical after every step.
- t1–t6 and seed/kernel agreement (bk1–bk4) green.
- ASAN-clean self-compile.
- **Peak RSS measured and materially reduced** at each committed step, with a
  documented before/after. There is no hard target number; the direction and
  magnitude (toward the seed's 0.22 GB) is the goal. The ~40x gap is the
  headroom.
- The uncommitted `&HExpr`-accessor / match-walk change that was already in the
  tree is committed as `2aaa847` (baseline for this work).

## 7. Non-goals

- New language features beyond the existing `&T`/`&` support.
- Changing any data structure (tables stay cons-lists) unless lever C is
  justified by measurement.
- Changing `clone`/`drop` semantics, HAMT refcounting, or collection layouts.
- General lifetime/region inference.

## 8. Risks

- **Removing a needed clone** → ownership error at build (safe, revert); the
  checker is the guard.
- **Fixpoint divergence** → a removal that is accepted but changes generated
  behaviour is caught by `k2.ll != k3.ll` or the fixtures.
- **Boxed-scrutinee alias rules** are subtle (`crates/cato-core/src/hir/ownership.rs`
  "boxed-scrutinee arm" / binder-alias retirement); some removals will be
  rejected. Rejection is fine — leave the clone.
- **RSS dominated by live set, not churn** → the spike in §5.1 measures this
  directly before the bulk of the work.
