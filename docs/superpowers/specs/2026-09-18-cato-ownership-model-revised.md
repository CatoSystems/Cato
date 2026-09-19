# Cato Ownership Model — Revised Canonical Design (v2)

Date: 2026-09-18
Status: **Supersedes** `2026-09-18-cato-ownership-drop-model.md` (v1, the reviewed
draft).

> **Implementation status (this change).** Milestones M1, M6, and the core of M2 are
> implemented:
> - **M1 typed HIR:** `Bin`/`Cmp`/`ConvF64` are gone from HIR; lowering emits
>   `BinW`/`BinF`/`CmpW`/`ICast(from, to)` directly. Mixed-width arithmetic and
>   comparisons are compile errors (`E202`). The interpreter no longer reconstructs
>   numeric semantics from runtime values (`ty_of_val` deleted; `ICast` carries the
>   source width, which also removes the signed-only unsigned→float interp behavior).
> - **M6 borrow simplification:** `&!` and `E302` removed; `&` call-boundary shared
>   borrows remain, with the moved-borrowed-param rule (`E303`).
> - **M2 core — ownership-driven destruction:** the shape heuristic
>   (`fresh_owning_value`/`annotate_frees`) is replaced by the state-driven
>   `plan_drops` pass: per-binding buffer ownership tracked through moves; a binding
>   frees at scope exit iff it still owns a heap buffer. Move transfers (let-of-var,
>   conj sources, owned call args, closure captures, explicit `drop`) suppress the
>   source's drop and hand the duty to the heir — fixing a confirmed `--no-opt`
>   double-free (a moved param freed by its callee when the value was returned;
>   regression: `examples/mem/moved_param_return.cato` + two sanitizer/IR tests).
>   The planner also closes leak classes the shape heuristic missed (`clone`,
>   `ToStr`, `slurp`, `parse-*`, `args`, inline-owning constructors).
> - **Subsequently implemented (same change series):** M2-remainder — **persistent
>   collection semantics**: `conj` no longer consumes its vector; last-use analysis
>   (exact occurrence counting) sets the node's `in_place` flag so codegen appends
>   into the source's buffer only when the source is provably never read again,
>   and copies otherwise (the source stays valid). **M3** — closures are move-only
>   owning values (`{code, env, drop}` header, per-lambda `@drop__lambda_N` glue,
>   ownership bitmap word in each env so the glue frees only captures the env
>   actually owns — static literals and aliases are excluded); lend closures with
>   `&` params. **M4** — HAMT nodes carry refcounts (`hamt_retain`/`release_rec` in
>   runtime.c; assoc retains shared siblings; `cato_map_release_pair` frees flat
>   or walks the trie) — `examples/hamt.cato` is ASAN-clean. **M5** — two-argument
>   `get`/`nth` return `(Option V)` (branchless `{tag,payload}` materialization,
>   `cato_hamt_find` for HAMT presence); explicit-default forms unchanged;
>   reference: `examples/options2.cato`. Also fixed: `(drop x)` now actually frees,
>   zero-parameter closure calls emit valid IR, and owning ADTs with vector fields
>   deep-copy the field buffer at construction (strdup rule widened) and bind
>   without dominance errors.
> - Still open: per-arm scratch drops (a loser `if`/`match` arm whose value
>   *allocates* leaks that allocation — common shapes take the transfer path and
>   are clean); explicit `Drop` nodes as a distinct IR form (the `Let` free list
>   is the drop block; recomputed per optimizer round); shadowed-name env slots.

This revision was produced by re-deriving the model from the repository facts rather
than defending v1. Repository facts are marked **[fact]**; everything else is proposed.
The driving instruction: *the simplest ownership model that provides strong memory
safety without GC, whose rules a Cato programmer can hold in their head.*

---

## 1. Executive assessment

**Keep from v1:** ownership-driven (not shape-driven) destruction as the goal; the
move-checked core; per-type monomorphized drop glue as the only destruction mechanism;
closures as move-only values with owned environments; per-node refcounting for HAMT
maps, hidden inside the collection implementation; `Option`-returning `get`/`nth`;
typed HIR with widths carried in the IR; differential fuzzing; the staged migration
with ASAN gates.

**Change:** `conj`/`assoc`/`disj` become *uniformly persistent* in language semantics,
with in-place linear update as a compiler decision (uniqueness at last use) — not a
user-visible consume. Merges and statement temporaries become *complete by
construction* (per-arm and end-of-statement destruction as semantic rules), which lets
uncertainty be a compile error instead of a leak state. Destruction becomes explicit
HIR (`Drop` nodes) inserted once, after which the optimizer sees ordinary code — no
ownership metadata, no re-running of ownership analysis per rewrite round. Typed
arithmetic comes from lowering directly, not a canonicalization pass.

**Remove:** allocation identities (α) and ownership sets (binding slots are the
identity; no per-allocation bookkeeping is needed); the `Leaked(bucket)` state; the
`Dead`/`Moved` distinction (one state); the entire lexical borrow-region apparatus and
`&!` (v1's §E.5 and E.6 both go); `fresh_owning_value` as a decision procedure (it
survives only as the two-line rule for Copy `str`); `recompute_frees`, `type_ops` as a
separate pass, and per-round ownership recomputation in the optimizer.

The pivotal finding that drives the simplifications **[fact]**: the shipped checker
already has two unacknowledged collection semantics — `conj` moves its vector (linear)
while `assoc` borrows its map (persistent, `ownership.rs:168-172` vs the `Conj` arm).
v1 formalized this split; v2 removes it: **one collection semantic (persistent
values), one optimization rule (mutate in place iff the source is provably never read
again)**. The user-facing model shrinks to four states, three operations that consume,
and one destruction rule.

## 2. Problems in the v1 proposal

1. **Allocation identities were over-engineering.** α-sets solved problems the binding
   discipline already solves: at codegen the *binding's slot* is the allocation
   identity; moves transfer the slot's content; per-arm drops are arm-local. Nothing
   downstream ever needed to name an allocation — only to know *whether* a binding
   still owns one. That is a boolean per binding, which the move checker already
   tracks.
2. **`Leaked(bucket)` was an escape hatch, not a state.** Under scrutiny the two v1
   buckets dissolve: merges are handled exactly by per-arm destruction (a semantic
   rule, not conservatism), and statement temporaries by end-of-statement destruction.
   What remains genuinely unprovable should be **rejected at compile time** (pre-1.0),
   not leaked with a counter. A sound model that silently leaks is a contradiction in
   its own terms.
3. **`conj`-consumes made the language lie about itself.** Cato advertises
   immutable-first functional style; rejecting `(count v)` after `(conj v 1)` is a
   linear type wearing a Lisp's clothes. The observable-behavior view (§7 of the
   review): a functional collection operation has persistent semantics; in-place
   update is *unobservable* exactly when the source is never read again — which is a
   compiler provable fact, so it belongs to the compiler, not the semantics.
4. **Borrow regions bought nothing.** Cato's values pass by handle (a vector is a
   3-word header, a boxed ADT a pointer — moves are already free **[fact]**), so
   borrows are not a performance feature; their only real job was licensing `&!`
   mutation, which uniqueness-driven conj makes unnecessary. v1's lexical regions,
   borrow expressions, and escape analysis were machinery for a language with pervasive
   mutation. Cato doesn't have one (no `set!`, no interior mutability — S1 §6
   non-goals **[fact]**).
5. **Optimizer re-running ownership analysis each round was architecturally messy**
   (v1's §D). It exists only because frees lived in side-tables. Making destruction
   explicit IR removes the need: the optimizer transforms ordinary code under one
   local invariant (§10).
6. **Missing semantics in v1:** loops with owning accumulators were postponed without
   a model; per-arm scratch destruction was deferred as a "bucket lift"; the `&!`
   removal consequence for E302 was unexamined; `(Vector str)` element ownership was
   undischarged. All resolved below.
7. **`Dead` vs `Moved` duplicated one concept.** Both mean "the affine resource is
   gone"; diagnostics can name the event without a second state.

## 3. Revised ownership model

### 3.1 Values and states

```text
Copy types     scalars, bool, str            untracked; assignment/args copy
Non-Copy types Vec, Map, Set, Adt, Fn        tracked

Binding state  σ ::= Own | Gone
Borrow state   (during a call only) Borrowed — usable, immovable, undroppable
```

Four words total: **Copy, Own, Gone, Borrowed**. There is no allocation identity, no
provenance in the model, no sets, no leak state. Provenance survives in exactly one
narrow, honest place: a `str` binding frees iff its initializer *allocated* (the rule
of §5.3) — because `str` is Copy, aliasing must never free, and only allocation sites
may.

### 3.2 The three consuming operations

1. **Move** — `let`-binding, argument pass (owned param), constructor field, closure
   capture, `recur` rebind, return. Source → `Gone`; recipient → `Own`.
2. **Borrow** — argument pass (`&` param). Owner keeps `Own`; immovable for the call.
3. **Drop** — implicit (scope exit / statement end / losing arm) or explicit (`drop`).
   Requires `Own`; produces `Gone`. `use` after any of the three = `E301`.

Everything else reads without state change. There are no other consuming operations in
the language, and the design commits to adding none without revisiting this list.

### 3.3 Collections are persistent — uniformly

**Language semantics.** All collections are immutable values. `(conj v x)`,
`(assoc m k v)`, `(disj s x)`, `subvec`, `split`, `subs`, `join` return *new* values;
the source remains valid, unchanged, and usable. `v` after `(conj v 1)` is the old
vector, and reading it is legal. This is today's *map* semantics **[fact]**, extended
to vectors and sets.

**Implementation rule (the only place uniqueness appears).** A collection operation
may update its source's buffer **in place** iff, at the operation site, the source
binding provably never reads again — i.e. the operation is the binding's last use and
subsumes its scope-exit drop. Then the source's drop entry *transfers* to the result
(the mechanism exists today as clone elision **[fact]** and is here generalized from
`(clone x)` to every collection operation). Otherwise the operation copies. The
choice is unobservable: when the source is dead, mutating its buffer changes nothing
anyone can see; when it is live, the copy preserves the snapshot.

This is the whole uniqueness story: not a user-facing linearity discipline, just
last-use analysis feeding a copy-or-mutate decision — the "strong compiler analysis,
simple rules" the review brief asks for. `&!` is thereby *unnecessary*: its sole
purpose was licensing the in-place path that last-use analysis now licenses for free.

### 3.4 Destruction — complete by construction

Every allocation's fate is one of exactly four, all statically placed:

1. **Owned by a binding** → dropped at the binding's scope exit (or explicit `drop`,
   or consumed by move into something that will drop it).
2. **Escaped** (returned / passed to an owned param / captured) → the recipient's
   responsibility by rule; the giver does not drop.
3. **Branch temporary** → a losing arm's value is destroyed at arm exit.
4. **Statement temporary** → an unnamed intermediate (`(do (subs s 0 2) 1)`) is
   destroyed at the end of its statement.

If a future construct cannot be phrased in these four, it is a **compile error**
("cannot determine the destruction point of this value") until the analysis covers it.
Uncertainty is rejection, not leakage. The v1 `Leaked` state does not exist; the only
remnant of "conservative" is the existing per-family contracts that are *documented
semantics* (below), not analysis escapes.

## 4. Revised borrowing model

```text
& T            a parameter annotation: read-only, call-scoped lend
```

- One borrow form. No `&!` (removed pre-1.0; `E302` exclusivity checking goes with
  it — its only client was double-`&!`).
- Borrows exist at **call boundaries only**: declared on parameters, checked at
  arguments. No borrow *expressions*, no lexical regions, no lifetime variables, no
  stored references, no returned references (permanently, unless a zero-copy view
  feature someday justifies real regions — the need is small precisely because values
  pass by handle: a vector move copies three words **[fact]**).
- The soundness duty is minimal: a borrowed parameter cannot be moved, captured, or
  dropped inside the callee (today's `E303` **[fact]**), and the callee's `&` params
  are never freed (`annotate_param_frees` already skips them **[fact]**).
- What borrows are *for*, honestly stated: read-only intent ("this callee promises
  not to consume") and FFI (`extern` params are all `&` **[fact]**). Not performance.

## 5. Revised destruction model

### 5.1 When values are destroyed

| Site | Rule |
|---|---|
| Binding scope exit | drop iff state is `Own` (move/consume/return suppress it) |
| Explicit `(drop x)` | immediately; `x` → `Gone` |
| `let`-of-`Var` | transfer: source suppressed, binding drops instead |
| Collection op on dead-after source | source's entry transfers to the result (§3.3) |
| Losing `if`/`match` arm | arm's value dropped at arm exit |
| Statement end | unnamed temporaries dropped |
| `recur` rebind | old loop value dropped unless the new value's computation consumed it (transfer) |
| Loop exit | final bindings drop (unless returned — `loop` yields its value) |
| Callee body exit | owned params drop unless returned (today's `annotate_param_frees` **[fact]**) |
| `exit` / process end | OS reclaims; no drop sequencing (documented) |

### 5.2 Drop mechanics

Unchanged from the shipped system, which is already right **[fact]**: destruction is
*monomorphized, per-type, compiler-generated glue* — `@drop__TypeName` for owning
ADTs (fields in reverse declaration order, then the box), string/vector/map/set frees
in `emit_let_frees`, and now `@drop__lambda_N` for closure envs and a `cato_map_release`
walk for HAMT roots. No runtime dispatch, no destructor tables; LLVM inlines the glue.
Destruction is mechanically derivable from type structure — the review's §11 goal —
with one representation rule per family (§7).

### 5.3 The `str` rule (kept, narrowed, honest)

`str` is Copy **[fact]**; a binding frees its buffer iff its initializer was an
allocation site (`subs`/`split`/`join`/`str`/`char->str`/call); initializers that are
literals or `Var` aliases never free. This is two lines of the current
`fresh_owning_value`, and it is *sound* precisely because `str` is Copy: aliases must
never free, and only fresh sites allocate. It is not worth a runtime provenance bit or
allocation tracking. Collections hold only heap strings — there is no `vec-str`
literal constructor **[fact]** — so a `(Vector str)` drop frees every element.

## 6. Revised closure model

```text
Fn value = { code ptr, env ptr }        [fact, unchanged]
env      = an owned allocation holding the captures
```

- **Closure values are move-only.** The binding owns the env; moving the closure
  transfers the env; the env drops when the binding drops (`@drop__lambda_N`: run
  captures' glue in reverse slot order, free env). The Copy treatment shipped today
  **[fact]** is retired with the S2 leak contract it existed to serve.
- **Captures**: by move for non-Copy values (already the checker's rule **[fact]**);
  by copy for scalars and static strings. Capturing a persistent collection shares
  nodes per §7 (the collection moves into the env; its glue is the release walk).
- **Calls do not consume**: `ClosureCall` borrows the binding (multiple invocations
  are legal, as today **[fact]**). Passing a closure to an owned parameter moves it.
- **Returned closures** transfer the env to the caller; the callee does not drop
  (escape rule).
- **Closures in collections / ADT fields**: requires `Elem::Fn` / `Fn`-typed fields,
  which do not exist yet **[fact]**; when added, glue composition handles them (the
  collection's element glue calls the lambda glue). No redesign needed — this is the
  compositionality test the model passes.
- **Recursive closures**: unsupported; a closure cannot capture itself (the binding
  does not exist while its initializer is checked). Named `defn` recursion is the
  recursion story **[fact]**. Documented limitation, not an oversight.
- **Closure inside closure**: envs nest naturally; inner env is a capture like any
  value.

## 7. Revised persistent-collection model

One family rule, all hidden below the value level:

| Family | Operation semantics | Internal sharing | Destruction |
|---|---|---|---|
| Vector | persistent `conj`/`subvec`; in-place iff source dead-after (§3.3) | none (copy or unique-linear update) | free flat buffer |
| Set | persistent `conj`/`disj`; same uniqueness rule | none | free flat buffer |
| Map (flat ≤ 8) | persistent `assoc`; budget copy | none | free flat buffer |
| Map (HAMT) | persistent `assoc`/`dissoc`; **new path, shared siblings** | per-node refcount | drop root → release walk; node frees at count 0 |

- Refcounting is an **implementation detail of the map family**, invisible to the
  ownership model: a map is one move-only owning value; the language-level system
  never sees nodes. It lives in `runtime.c` helpers + drop glue — not in the type
  system, not in the checker (the review's §10 question answered: hidden inside the
  collection implementation).
- Counts are non-atomic while values are single-threaded (today's parallelism is
  vector-only **[fact]**); switching to atomic counts is a runtime flag, not a redesign.
- HAMT is a DAG (no cycles), so release walks terminate — no Rc-cycle hazard.
- `assoc` on a provably-unique map may elide counts entirely later (compiler-assisted
  uniqueness, §3.3's rule applied one level down); semantics unchanged. This is the
  only place the "E: compiler-assisted uniqueness" option survives, and it is an
  optimization, never the mechanism.

## 8. Revised typed HIR

The invariant, stated once: **post-lowering HIR carries the full numeric semantics;
neither backend infers anything from runtime values.**

- `Bin` and `Cmp` are deleted from HIR. Lowering emits `BinW`/`BinF`/`CmpW`/`ICast`
  directly — lowering already computes operand widths at these sites (`promote_pair`,
  `lower_cmp`) **[fact]**, so no separate `type_ops` pass is needed; the canonical
  form is the only form.
- Arithmetic is **wrapping at every width** (today's behavior **[fact]**), documented;
  `i64::MIN / -1` is pinned to wrapping; the fuzzer holds both backends to it.
- Comparisons carry width + signedness (`CmpW.ty` → `ult` family vs signed) **[fact]**;
  the interpreter's unsigned branch (shipped) becomes the only path.
- Integer↔float mixing is a **compile error** ("cast explicitly"), matching the
  existing f32 strictness **[fact]**. The interpreter loses `ty_of_val` and the
  float-when-either rule entirely **[fact: both deleted]**.
- Constants: `ConstI64`/`ConstF64` with `ICast` inserted at narrow bind points
  (today's `wrap` **[fact]**, now uniform).

## 9. Revised compiler pipeline

```text
read → expand → parse
  → lower        typed HIR directly (widths in BinW/CmpW); monomorphization
                 interleaved as today [fact: instantiate lowers per call site]
  → owncheck     ONCE, on the fully lowered (post-mono) program:
                 - state machine over Own/Gone/Borrowed
                 - last-use analysis for collection ops (§3.3)
                 - drop placement, emitted as explicit HIR Drop nodes at
                   scope exits, arm exits, statement ends, rebind points,
                   with transfer annotations (let-of-var, dead-source conj)
  → optimize     plain HIR rewrites (fold, simplify/DCE, devirt, inline).
                 Drop nodes are ordinary pure statements; no ownership
                 metadata exists to preserve; nothing is recomputed.
  → glue gen     @drop__T / @drop__lambda_N / map release (unchanged shape)
  → backends     LLVM emits Drop nodes as frees; interp unchanged (Rc [fact])
```

Ownership analysis runs **once**, before optimization, because its entire output is
code — not metadata. The optimizer's single invariant (§10) replaces v1's
recompute-per-round architecture, and today's `recompute_frees` + free-list side
tables **[fact]** are deleted. `verify_ownership` survives as a debug-only test
assertion, not pipeline structure.

## 10. Revised optimization strategy

**Invariant OPT-1: a `Drop` may be deleted only together with the death of its value,
and never duplicated onto multiple paths.**

Under this invariant the existing passes are simply correct:

- **DCE** deleting an unused `conj` chain deletes the allocation *and* its Drop
  together — fewer allocations, no leaks.
- **Copy-prop / folding** never duplicate Drops (walks are conservative rewrites
  **[fact]**); inlining copies bodies including their Drops verbatim.
- **Clone elision** is subsumed: `(clone x)` with `x` dead-after is a move + transfer
  of x's Drop to the destination — the same rule as dead-source `conj` (§3.3), one
  mechanism instead of two.
- Branch simplification that removes an arm removes the arm's Drops with it.
- Scalar replacement / allocation elimination are future passes; OPT-1 is their
  contract too (delete allocation ⟹ delete its Drop).

No pass reasons about ownership; ownership analysis never reruns. This resolves the
review's Option A–D question: **drops are explicit IR (a form of Option A) without
ownership metadata (avoiding Option C's cost), and nothing is recomputed (rejecting
Option D).**

## 11. Revised formal rules

Judgment: `Γ ⊢ e ⇓ v, drops` — expression `e` in env `Γ` (name → state) yields a value
kind and the drops it places. `fresh(e)` marks allocation-site expressions (§5.3's
list). `last(x, at e)` means `x` has no occurrence after `e` in the enclosing body.

```text
states      σ ::= Own | Gone | Borrowed
types       Copy = {scalars, bool, str}     Own-able = {Vec, Map, Set, Adt, Fn}

(LIT)   Γ ⊢ lit ⇓ Copy
(VAR-r) Γ(x)=Own   ⊢ x ⇓ Own            (read: state unchanged)
(VAR-m) Γ(x)=Own   move x ⇓ Own, Γ[x↦Gone], drop(x) suppressed

(LET)   Γ⊢e⇓Own   Γ[x↦Own] ⊢ body      drop x at exit iff σ(x)=Own
(LET-M) Γ⊢x⇓Own   let [y x]: Γ[x↦Gone][y↦Own]   (transfer: §3.2-1)

(CONJ)  Γ(v)=Own, v non-Copy:
          last(v, at (conj v x))  ⇒ in-place: v ↦ Gone, result Own   (§3.3)
          otherwise               ⇒ copy:    v ↦ Own,  result Own
        (maps/sets identical rule; HAMT sharing is internal, §7)

(CALL)  owned param:  arg Own → Gone (callee inherits destruction duty)
        & param:      arg Own → Borrowed for the call; callee may not
                      move/capture/drop it (E303); owner unchanged after
        result:       Own if non-Copy (caller drops unless it escapes)

(IF)    each arm: Γarm ⊢ arm ⇓ v
        taken arm's value is the result; losing arm's value dropped at
        arm exit                                  (branch-temporary rule)

(MATCH) scrutinee borrowed, not consumed [fact]; binders are reads of the
        variant's fields (no field moves in v1 — clone to keep); arm rule
        as (IF)

(SEQ/STMT) an unnamed non-Copy temporary is dropped at statement end

(LOOP)  binds must be Copy or Own; at (recur args…):
          old value of each var: dropped, unless consumed by transfer
          from the corresponding arg's evaluation
        loop exit: completing value per (IF)-merge rule; vars dropped

(RET)   tail value escapes; every binding's drop suppressed by escape

(CLONE) Γ(x)=Own ⇒ x unchanged, fresh Own result (both drop)

(DROP)  Γ(x)=Own ⇒ Γ[x↦Gone], immediate glue; else compile error

(FN)    captures: non-Copy free names move ( Gone), Copy names copy;
        binding Own(env); glue = captures' glue then free env

(COPY)  Copy types: every rule above is a no-op; drop(str) fires only for
        allocation-site initializers (§5.3)
```

Soundness argument in one line: every allocation site's value enters exactly one of
the four fates of §3.4; each fate places exactly one drop on every linear history;
branch and statement temporaries are dropped at *their* points, so no path accumulates
an unowned allocation; any construct outside the rules is a compile error.

## 12. Safety invariants

```text
S1  No use-after-move.                (E301; checked once, pre-opt [fact])
S2  No double-free.                   (each fate places ≤1 drop per history;
                                       OPT-1 forbids duplication)
S3  No use-after-free.                (moves/borrows/§3.3 transfer make
                                       dangling impossible; E303 for lends)
S4  One destruction authority per owned allocation. (the owning binding,
                                       or the unique heir after transfer)
S5  Every destruction consumes ownership. (Drop requires Own → Gone)
S6  Borrows cannot outlive the owner. (call-scoped by construction)
S7  Moved values cannot be used.      (= S1)
S8  Static memory is never freed.     (str rule §5.3; literals unreachable
                                       from any drop list)
S9  Shared persistent nodes freed only at last owner. (release walk, §7)
S10 Interpreter ≡ native semantics.   (typed HIR §8 + differential fuzzing)
S11 Optimization cannot invalidate ownership. (OPT-1; drops are plain code)
S12 Closure environments die deterministically. (move-only + glue, §6)
S13 Unreachable temporaries are destroyed at their point. (arm/statement
                                       rules — the completeness guarantee)
S14 In-place update is unobservable.  (fires only when the source is
                                       provably never read again, §3.3)
S15 Uncertainty is a compile error, never a silent leak. (§3.4)
```

## 13. Implementation plan

Same TDD/ASAN/parity method as the shipped slices. Order re-justified: destruction
(M2) before closures (M3) because envs are just another owned allocation; typed HIR
first because everything downstream consumes it; fuzzing rides with M1.

```text
M0  Groundwork (no behavior change)
    leak-inventory report; pin parity snapshot; add examples/errors cases
    for the new diagnostics.

M1  Typed HIR + differential fuzzer, Tier 1
    - lower emits BinW/BinF/CmpW/ICast directly; delete Bin/Cmp nodes
    - delete interp's ty_of_val + float-when-either; int↔float mixing = error
    - crates/cato-fuzz (std-only): width/cast/corner generator, both
      backends, seed-logged, minimized repros → examples/fuzz_corpus/
    Modules: hir/lower.rs, interp/lib.rs, new crates/cato-fuzz
    Tests: widths suite; fuzz corpus; negative mixed-arithmetic tests
    Risk: promotion-relying examples break (mechanical fixes)

M2  Ownership-driven destruction
    - owncheck v2: Own/Gone/Borrowed + last-use analysis
    - drop placement as explicit HIR Drop nodes (scope/arm/statement/rebind)
    - persistent collection semantics: conj/assoc/disj no longer move;
      copy-or-in-place by last-use; entry transfer on in-place
    - delete fresh_owning_value, recompute_frees, annotate_frees outputs
    - per-binding env slots in the LLVM backend (no shadow-slot sharing)
    Modules: hir/ownership.rs (rewrite), hir/build.rs, opt/mod.rs,
    llvm/heap_vec.rs, llvm/exprs_flow.rs
    Tests: transfer cases ((let [y x]), conj chains), per-arm drops, ASAN
    corpus with --opt and --no-opt, loop rebind drops
    Risk: codegen slot discipline (shadowing) — gate on output parity; IR
    will legitimately change

M3  Closure ownership
    - closures move-only; @drop__lambda_N; capture moves wired through M2
    - examples updated (grep shows low exposure)
    Tests: closure created in a loop → flat RSS under ASAN (the acceptance
    test); use-after-move of closure values
    Risk: Copy-closure reliance → clone or restructure; diagnostics guide

M4  Map release walk (HAMT refcounts)
    - counts in nodes; assoc/get inc shared children; drop = release walk
    - delete the flat-only special case in emit_let_frees [fact]
    Modules: runtime/runtime.c, llvm/heap_map.rs, heap_vec.rs
    Tests: hamt example under leak-checking ASAN (flips the documented
    leak contract); share/drop orderings
    Risk: assoc-heavy perf (measure; counts are cache-local)

M5  Option get/nth
    - 2-arg → Option (existing monomorphized ADT); 3-arg unchanged
    Tests: parity on new forms; every example call site updated once
    Risk: none structural

M6  Borrow simplification
    - remove &! and E302; keep & params; update own.cato/specs
    Tests: existing own.cato minus peek2; new negatives
    Risk: trivial (feature removal)

M7  Fuzz Tier 2
    - transfers, borrows, collections, closures, loops in the grammar;
      acceptance-parity (both backends reject the same programs)
```

Sequencing note: M2 is the load-bearing phase; M1 exists largely to make M2's
regression net strong. Nothing in M0–M7 requires the borrow regions, allocation
identities, or leak buckets that v1 carried.

## 14. Tests

The ownership suite (new: `examples/errors/` negatives + `examples/` positives, all
run through *both* backends with byte-identical output, ASAN-clean with and without
optimization):

```text
Ownership     move; double move (E301); use after move; conditional move
              (both-arm merge); nested moves; shadowing (per-slot codegen);
              return transfer; owned-arg transfer; loop rebind transfer +
              drop; statement-temporary drop; branch-temporary drop
Borrowing     & param read; move of borrowed param (E303); capture of
              borrowed param (E303); drop of borrowed param (E303)
Closures      capture move; capture copy (scalar); multiple invocations;
              returned closure (caller drops); closure dropped in loop
              (flat RSS); nested closures; closure capturing a map (release)
Collections   conj keep-old (copy path: old unchanged, both drop); conj
              dead-after (in-place: one drop); assoc keep-old (HAMT share +
              counts); assoc chains; disj; subvec; (Vector str) drop frees
              elements; set drop
ADTs          boxed recursive tree build/drop (trees.cato); inline owning;
              Option of scalar and of owning type; match borrows scrutinee
Control flow  if/match merges both directions; early recur paths; loop
              exit with live accumulator; exit() skips drops (documented)
Diagnostics   every E-code above asserted via the errors/ suite
Fuzz          Tier 1 (M1) numeric/parity; Tier 2 (M7) adds transfers,
              borrows, collections; acceptance-parity for invalid programs
```

## 15. Open questions

1. **`exit` sequencing.** Should `exit` run drops for the exiting function's owned
   bindings? Today it does not **[fact]** (OS reclaims). Cheap to keep unspecified;
   becomes user-visible only if drop ever gets observable side effects, which the
   no-user-destructor rule forecloses. Lean: keep unspecified, document.
2. **Field-level moves out of ADTs** (`(match p (Pair [a b] …))` consuming one field
   while the parent lives). v1 and v2 both defer to clone; a partial-drop design
   (per-field owned bits) is real work with no current kernel need. Decide when the
   self-hosted kernel demands it.
3. **Exact refcount placement** (in-node word vs side table) — a runtime.c
   measurement question for M4, not a design question.
4. **Concurrency markers** (Send/Sync analogues). The model is concurrency-ready —
   ownership transfer is a send, persistent values are read-only shareable, there is
   no interior mutability — but marker-type syntax, atomic counts, and channels are
   undesigned. Do not design until threads are a goal; nothing here blocks them.
5. **`loop` variables of `Fn` type** (closure accumulators): mechanically fine under
   the loop rule, but useless until `Fn` values can round-trip codegen fully; fold
   into the `Elem::Fn` future slice.

---

## 25. Final decision table

| Proposal (v1) | Keep | Modify | Remove | Reason |
|---|---|---|---|---|
| Ownership-driven (not shape-driven) destruction | ✔ | | | the correct goal; ship it |
| Allocation identities α + ownership sets 𝒜 | | | ✔ | binding slots are the identity; nothing needed to name allocations |
| `Dead` vs `Moved` as separate states | | ✔ | | unify as `Gone`; diagnostics name the event |
| `Leaked(bucket)` state | | | ✔ | merges/temporaries are complete by construction (§3.4); true uncertainty = compile error (S15) |
| Per-arm + statement-temporary destruction | ✔ | | | promoted from "bucket lift" to semantic rule |
| `fresh_owning_value` as decision procedure | | ✔ | | deleted as procedure; survives as the 2-line Copy-str rule (§5.3) |
| `conj` consumes its source (linear vectors) | | | ✔ | persistent semantics for all collections; linearity becomes compiler-internal last-use (§3.3) |
| In-place linear update | ✔ | | | kept as the compiler's unobservable optimization |
| Lexical borrow regions, borrow expressions | | | ✔ | no storable refs, no mutation surface; machinery without a client |
| `&!` exclusive borrows + E302 | | | ✔ | uniqueness-driven conj removes its only purpose |
| `&` call-boundary shared borrows (E303 rule) | ✔ | | | the whole remaining borrow system; minimal and sound |
| Closures move-only with owned envs + glue | ✔ | | | correct; S2 leak contract retired |
| Borrowed captures | | | ✔ | deferred indefinitely; move-captures + `clone` suffice |
| HAMT per-node refcounting | ✔ | | | hidden entirely inside runtime.c + map glue |
| Arena-based HAMT | | | ✔ | contradicts persistence (`assoc` outlives source) |
| Compiler-assisted HAMT uniqueness | ✔ | | | demoted to future optimization, never the mechanism |
| Monomorphized per-type drop glue | ✔ | | | already shipped and correct [fact] |
| `Option` get/nth (2-arg) + explicit-default (3-arg) | ✔ | | | the loudest safety win, zero new machinery |
| Typed HIR (BinW/CmpW/BinF/ICast only) | ✔ | | | widths in IR; interpreter never infers |
| `type_ops` canonicalization pass | | | ✔ | lowering emits typed nodes directly; no pass needed |
| owncheck rerun per optimizer round | | | ✔ | drops are explicit IR; OPT-1 suffices; analysis runs once |
| `recompute_frees` / free-list side tables | | | ✔ | replaced by Drop nodes in HIR |
| Clone elision as separate pass | | ✔ | | subsumed by the transfer rule (one mechanism) |
| Differential fuzzing (2 tiers, corpus) | ✔ | | | the conformance jury; rides with M1 |
| Migration phases M0–M7 | ✔ | | | order re-justified; all v1-only machinery gone |

### The revised canonical design, in one paragraph

Cato's ownership model is four states — **Copy, Own, Gone, Borrowed** — three
consuming operations — **move, borrow, drop** — and one destruction rule: **a binding
that still owns its value when its scope ends, drops it**; branch losers and statement
temporaries drop at their own points; everything a value escapes to inherits the
destruction duty. Collections are persistent values with one uniform semantic; the
compiler mutates in place exactly when it can prove no one will ever look (last-use),
transferring the drop duty, and refcounts the one family (HAMT) whose sharing is real
— hidden below the value level. Closures own their environments and die
deterministically. Arithmetic carries its width in the IR, so both backends execute
one semantics, held honest by a differential fuzzer. What the compiler cannot prove,
it rejects — never silently leaks. No collector, no pauses, no metadata tables, no
lifetime annotations: destruction is compiler-generated glue derived from type
structure, and the whole model fits on an index card a Cato programmer can reason
from.
