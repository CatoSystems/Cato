# The Cato Ownership & Deterministic-Destruction Model

Date: 2026-09-18
Status: **Superseded** by `2026-09-18-cato-ownership-model-revised.md` (v2), which
re-derives the model after review. Retained as the reviewed draft; its [fact]
annotations remain accurate repository observations.
Scope: evolve the shipped S1c memory model (value-shape-based scope-exit frees) into a
formally defined ownership + move + borrow + compiler-generated-drop system. No tracing
GC, no hidden collector, no GC pauses — destruction stays deterministic and lexical.

Repository facts are marked **[fact]** with file references; everything else is proposed
future design. Nothing here is implemented yet.

---

## A. Current-state analysis

### A.1 What is sound today **[fact]**

- **Move semantics with compile-time use-after-move rejection.** The flow-sensitive
  checker (`crates/cato-core/src/hir/ownership.rs`) tracks every non-Copy binding as
  `Owned | Moved` through `let`/`if`/`match`/`seq`/`loop`, consulting callee borrow
  kinds. `E301 use of moved value` is enforced both pre- and post-optimization
  (`opt/mod.rs: verify_ownership` re-runs the checker as a safety net).
- **Borrowed parameters cannot move.** `&`/`&!` parameters are non-movable for the whole
  callee body (`E303`): any attempt to pass them to an owned callee, capture them,
  `conj` them, or drop them is a compile error. This closed a demonstrated
  use-after-free (borrowed param → owned callee → callee's scope-exit free → caller
  reads a dangling buffer).
- **Deterministic destruction for the S1c value shapes.** `annotate_frees` annotates
  every `Let` with a free list; the LLVM backend emits the frees at scope exit
  (`heap_vec.rs: emit_let_frees`). Covered: fresh heap strings (`subs`/`split`/`join`/
  `str-cat`/`char->str`), fresh collections (literals, `conj`/`subvec`/`for` results,
  `assoc`), boxed-ADT constructions, and call results; plus owned move-type parameters
  at body exit unless tail-escaped (`annotate_param_frees`). Validated ASAN-clean over
  the whole `examples/` corpus.
- **Transitive drop glue.** `compute_adt_facts` derives boxed = cycle ∨ owning; every
  owning ADT gets a generated `@drop__TypeName` (`adt.rs: emit_adt_drop_fn`) that frees
  fields in reverse declaration order, recursing through children's glue, then frees the
  box. Strings join the owned world; literals stay static.
- **No double-frees.** The conservative rule ("anything uncertain leaks, nothing
  double-frees") plus the optimizer's discipline (`recompute_frees` every round;
  clone elision transfers free-list entries and never re-annotates afterwards) means
  no shipped path frees twice.

### A.2 What is conservative today **[fact]**

The free decision is **value-shape based**, not ownership-state based:
`fresh_owning_value` (`ownership.rs`) frees a binding only if its initializing
expression *syntactically looks like a fresh allocation*. Consequences:

- `let [y x]` where `x` owns a buffer leaks: the shape `Var` is excluded, so the free
  entry is dropped instead of transferred (clone elision only handles the `(clone x)`
  spelling).
- `let [y (if c a b)]` with both arms fresh leaks the loser and the winner: control-flow
  results are excluded outright.
- A binding whose value came from an element load, or whose freshness the shape walk
  cannot classify, leaks.

These are **memory leaks with no safety cost** — the invariant that is never violated is
soundness, only completeness.

Why the shape heuristic exists rather than a state-based rule **[fact]**: one aliasing
mechanism breaks a naive "drop everything still Owned at scope exit" rule — in-place
`conj`. `emit_conj` (`heap_vec.rs:10`) appends into the source's buffer when capacity
allows, so after `(conj v x)` the *result aliases the source's buffer* while both
bindings read as `Owned`. A state machine that drops both would double-free. Hence the
"never a `conj` source" exclusion, and hence shape-based freshness as the safe subset.
The second, milder aliasing mechanism is header copies: bindings share codegen env
slots when shadowed, so the free lists also exclude shadowed names.

### A.3 Known leaks (documented open contracts) **[fact]**

1. **Closure environments** are allocated once and never freed (`closures.rs` has zero
   `free` calls; closure values are treated as Copy; ownership.rs:6-8 documents the
   observational-soundness argument). Every non-fused higher-order call retains its env
   until process exit.
2. **HAMT interior nodes.** Maps are a `{ptr,len,tag}` header whose `cap` bit 0 selects
   flat-sorted-array vs HAMT root (`heap_map.rs:17`); scope-exit frees release only flat
   buffers (`heap_vec.rs: emit_let_frees`) — shared trie nodes live until process exit,
   per-node frees being unsafe under the sharing that `assoc` creates.
3. **Shape-uncertain values** (A.2): transfers, merges, and unclassified freshness leak.

### A.4 Known semantic limitations **[fact]**

- **Absence modeled as a value**: `(get m k)` missing → `0`; `(nth s i)` out of range →
  `0` (interp mirrors LLVM, so parity holds — but the C footgun is imported, and the
  default is evaluated eagerly in the interpreter while the backend has
  control-flow-sensitive default handling for vector `get` (`analysis.rs: has_ctl`), a
  known divergence corner the golden examples cannot see).
- **Numeric ops lose width in HIR**: `Bin`/`Cmp` carry no width; the float-promotion
  rule is re-derived at interpretation time (`interp: Bin` matches on `Value`), the
  unsigned `CmpW` family is a separate node, and `ty_of_val` reports `Ty::I64` for every
  integer — historically producing real interp↔LLVM divergences (u64 ordering;
  unsigned→float casts) that only manual corner tests caught.
- **`str` is one type with two shipped runtime shapes** (static NUL-terminated literal;
  owned heap buffer) distinguished by value freshness at drop time, plus a third
  aspirational shape in older docs.
- **Borrow `&!` mutation discipline, loop variables with owning types, non-Copy
  accumulators in `loop`, field-level moves out of ADTs**: all rejected or absent, each
  a documented slice limit, not a bug.

### A.5 Future design problems (not defects)

Borrow regions, closure env ownership, persistent-structure reclamation, user-defined
destructors (explicit non-goal in S1 §6), macro hygiene. This proposal covers the first
four; destructors stay a non-goal; macros are deferred (§E.7).

---

## B. Target memory model

The contract this design moves toward is the one S1 §2 already states as *language
contract* and the implementation approximates as *heuristic*:

> Every binding that owns heap memory is dropped at scope exit, unless it was moved,
> returned, or escaped. Dropping is deterministic, lexical, and transitive. If the
> compiler cannot establish a value's ownership story, it says so — with a diagnostic,
> not a silent leak.

### B.1 Allocation categories: the destruction story for each

| Category | Owner | Lifetime | Drop mechanism | Sharing |
|---|---|---|---|---|
| Scalar / bool | register/stack | enclosing frame | none | value copy |
| String literal | none (static) | program | none — provenance `Static` | aliasing (Copy) |
| Heap string | unique binding | binding scope | inline `free(ptr)` at scope exit or explicit `drop` | none (clone to share) |
| Vector | unique binding | binding scope | free flat buffer | none |
| Map (flat) | unique binding | binding scope | free flat buffer | none |
| Map (HAMT) | refcount per node | when last referencing map drops | root drop: recursively `unref`; free node at count 0 | **refcounted, exactly where sharing occurs** |
| Set | unique binding | binding scope | free flat buffer | none |
| Boxed ADT | unique binding | binding scope | `@drop__TypeName`: fields (reverse decl order) → box | none |
| Inline ADT w/ owning fields | unique binding | binding scope | drop glue over fields in place; storage dies with frame | none |
| Closure value | unique binding (move-only) | binding scope | env is an owned allocation: `@drop__lambda_N` frees captures' glue then the env | none (see §E.1) |
| FFI-returned data | C side | by C contract | never freed by Cato (v1: FFI returns scalars only) | — |

Two global rules close the table:

- **Provenance, not type, decides static-ness.** A `str` binding's drop is `free` or a
  no-op according to its allocation identity (`Heap(a)` vs `Static`), which the analysis
  tracks per binding — eliminating the "same type, different drop rules" wart without a
  runtime flag.
- **Every leak is a named, countable bucket.** Where the analysis cannot establish
  ownership at a merge or escape, the value lands in an explicit `Leaked(bucket)` state;
  `cato --dump-mem` reports the buckets per defn. The migration's progress metric is
  bucket count → 0.

### B.2 What the final model makes possible

```
Every heap allocation has exactly one owner at every program point.
Its destruction point is the earlier of:
  - its owner's explicit `drop`, and
  - its owner's scope exit (or move-out point, for in-place-consuming ops).
Sharing exists in exactly two forms, both statically named:
  - Copy aliasing (scalars, static strings), and
  - refcounted persistent-structure nodes (HAMT families only).
```

---

## C. Formal ownership rules

### C.1 Abstract values and binding states

A binding's abstract value carries the set of allocations it owns:

```text
α ∈ Alloc      allocation identities, minted per allocation site (SSA-like)
P ∋ ρ ::= Heap(α) | Static            provenance

V  ::= Copy                    no tracking (scalars; Static-provenance str)
     | Own(𝒜)                  owns the allocation set 𝒜 ⊆ Alloc, |𝒜| ≥ 1
     | Dead                    moved, released, or consumed (affine "gone")
     | Borrowed(κ)             only within a call: κ ::= shared | exclusive
     | Leaked(b)               conservative bucket b (merge/escape); sound, counted

σ(binding) ::= Unbound | V
```

`Dead` deliberately merges today's `Moved` with "explicitly dropped": both mean *the
affine resource is gone*. Diagnostics distinguish them by the consuming event, not by a
separate state. `Borrowed` is call-scoped today and lexically scoped under §E.5; a
borrowed binding is *usable but immovable* for the borrow's duration.

Allocation sets (not single ids) are needed because an inline owning ADT owns several
allocations (its children) without itself being a pointer: `Own({α_box})` for a boxed
ADT, `Own({α₁, α₂})` for an inline pair of heap strings.

### C.2 Transitions

```text
Own(𝒜) ──move (let / arg / capture / store)──►  Dead        receiver: Own(𝒜)
Own(𝒜) ──borrow─────────────►  Borrowed(κ) during the call, then Own(𝒜)
Own(𝒜) ──consume (owned call arg, in-place op)──► Dead
Own(𝒜) ──drop (implicit scope exit / explicit)──► Dead      emits D(𝒜)
Own(𝒜) ──clone──► Own(𝒜), receiver Own(𝒜′)      fresh allocations 𝒜′
Own(𝒜₁)∪Own(𝒜₂) arms ──merge──► Leaked(merge)  [v1; §E.6 lifts this]
Copy     ──any use──► Copy                       no state change, ever
```

**Dropping semantics** `D(𝒜)` is generated per concrete type — this is Part 2's
`drop : T → ()` made precise:

```text
D(Heap(α), T)  = dropglue_T(α)      Static-provenance members are skipped
D(Static, _)   = ε                   (no-op: literals, C-owned FFI data)
dropglue_T: scalars/str(Static) → ε
            str(Heap) → free
            Vec/Map(flat)/Set → free buffer
            Map(HAMT) → unref(root)            (§E.2; frees when count hits 0)
            boxed ADT → ∀ field f (reverse order): D(f); free box
            inline owning ADT → ∀ field f: D(f)
            Fn → D(env allocations); free env
```

Rules:

- **Implicit drop** occurs exactly at the drop point computed by the analysis (§D):
  the innermost enclosing scope exit of the owning binding, on every completing path,
  only while the binding's state is still `Own`. This is the *default*; no annotation,
  no freshness test.
- **Explicit `drop`** is permitted on any binding in state `Own` (or `Borrowed`'s owner
  is rejected — you cannot drop what you do not own; borrowed *parameters* were never
  the owner, which is why `E303` exists). `drop` consumes: the binding becomes `Dead`;
  using it afterwards is `E301`. Dropping an already-`Dead` binding is a compile error
  with its own message (`cannot drop x — it was already moved/dropped`).
- **`drop` never runs user code**: no user destructors (S1 §6 non-goal stands). Drop
  glue is fully compiler-generated; "drop" is a semantic operation of the *compiler's*
  ownership model, not a trait.
- **Partially initialized values do not exist in source.** Constructors are atomic:
  `Ctor` builds all fields, then the value is `Own`. The only partial state is inside
  generated glue (variant switch, then per-field frees), which is compiler-authored and
  ASAN-verified per type.
- **Variants**: drop glue switches on the runtime tag (already shipped in
  `emit_adt_drop_fn`); each variant drops exactly its own fields.
- **Recursive ADTs**: glue recursion terminates because the type is acyclic (cycles are
  not expressible as owning edges — S1 §2: self-owning structures leak by contract, and
  must route through non-owning ids).

### C.3 The judgment

Checking is a judgment over expressions in an environment of binding states:

```text
Γ; stash ⊢ e : (T, V, eff)
```

where `Γ` maps names to `V`, `stash` collects allocations dropped *inside* `e` (arm
exits, scratch values), and `eff` records escapes (call arguments, returns, captures).
Key typing rules, informally:

```text
(VAR)   Γ(x) = Own(𝒜)
        ├─ use in read position (count/get/match scrutinee): unchanged
        └─ use in consume position: Γ(x) := Dead, hand 𝒜 to the consumer

(LET)   Γ;⊢ e : (T, Own(𝒜))     x fresh
        Γ[x ↦ Own(𝒜)]; ⊢ body
        drop(x) at scope exit ⟺ σ(x) = Own at that point

(MOVE)  let [y x] with σ(x)=Own(𝒜):  Γ(x):=Dead, Γ(y):=Own(𝒜)     — transfer,
        replaces today's shape exclusion; the free entry moves with 𝒜

(CONJ)  (conj v x) with σ(v)=Own(𝒜), v non-Copy:
        Γ(v):=Dead, result Own(𝒜′)   — conj *consumes its source*.
        𝒜′ = fresh allocations; whether codegen mutates 𝒜 in place or
        reallocates is invisible above this line (the model only requires
        that v is dead and no other binding holds 𝒜 — guaranteed because
        𝒜 was uniquely owned).

(CALL)  ∀ (arg, kind): kind=owned ⇒ arg's V: Own(𝒜)→Dead, 𝒜 escapes to callee
                                      (callee owns 𝒜's destruction duty)
                     kind=shared/exclusive ⇒ Borrowed(κ) during the call
        result: Own(𝒜ᵣ) — the return value is fresh-or-transferred; the
        caller owns its drop.

(IF/MATCH) arms checked from the pre-branch state; a move in any arm is
        Dead afterwards (today's sound rule, kept). v1: results with per-arm
        allocation identities merge to Leaked(merge) unless one side is
        Copy/Static (§E.6 lifts this with per-arm scratch drops).

(LOOP)  loop variables are Copy (today's rule). Extending to owning
        accumulators requires the re-initialization discipline of §F (loops).
```

### C.4 What becomes a consequence of the model vs. stays heuristic

| Current rule in `annotate_frees` **[fact]** | Fate under the model |
|---|---|
| free only if type heap-owning | consequence of `Own(𝒜)` (𝒜 ≠ ∅ only for owning types) |
| never a call argument | consequence: call args consume/transfer; only `Own` at exit drops |
| never a `conj` source | consequence of `(CONJ)`: the source is `Dead` after conj |
| never the returned value | consequence: return is an escape that transfers 𝒜 |
| not shadowed | consequence: each binding gets a fresh env slot under its own α (codegen change, §D) |
| value shape must look fresh | **deleted** — replaced by allocation identity |

The shape walk survives only as the *minting* of allocation identities at the
allocation-forming expressions (the same list it knows today: string builders,
collections, ctors, calls) — but its verdict no longer decides anything; it only names
allocations. Everything downstream is state-driven.

---

## D. Compiler architecture

Pipeline (changed passes marked ★). Everything runs before backends; both backends
consume the same annotated HIR, and the interpreter needs no frees (its `Rc` values
already release at last use **[fact]** — value parity is unaffected).

```text
read → expand → parse
  → lower (HIR)                        unchanged
  ★ type_ops (Part 8): rewrite Bin/Cmp → BinW/BinF/CmpW/ICast per the
    promotion table, immediately after lowering, per-function. Bin/Cmp are
    then only lowering-internal. The interpreter's value-reconstruction
    (ty_of_val, "float when either is float") is deleted.
  → monomorphize (defns + ADTs)        unchanged
  ★ owncheck v2: the flow-sensitive state machine of §C — replaces
    check_ownership's Owned/Moved pairs and PRODUCES, per Let: the drop list,
    per-binding allocation provenance, and leak-bucket counts. Runs in
    build_program_located (pre-opt) and again as opt's verify step.
  ★ assign_drops: derived from owncheck's output (today's annotate_frees +
    recompute_frees become this pass's output stage). Drop lists carry
    allocation ids so the backend frees each α exactly once, at its owner's
    drop point; per-arm scratch drops are emitted at arm exits.
  → opt (fold → simplify → devirt → inline, fixed point)
      ★ recompute_frees → re-run owncheck+assign_drops each round (same
        slot as today's recompute_frees). clone_elide is subsumed: a clone
        whose source is dead afterwards is a transfer (the optimizer may
        rewrite (clone x)+dead-x to x as today, and owncheck's transfer rule
        makes it automatically leak-free either way).
  → drop-glue generation               unchanged for ADTs (adt.rs);
                                       ★ extended: @drop__lambda_N for envs,
                                       unref glue for HAMT nodes
  → backends
      LLVM: emit drop lists as today's free lists, plus: per-binding env
            slots (no shared slots across shadowed bindings — required by
            per-α frees), arm-exit scratch drops, @drop__lambda_N calls.
      interp: unchanged (Rc).
```

Where ownership vs. borrow vs. drop analysis live: **one pass** (`owncheck v2`) owns all
three, because they share the same state machine — borrowing is a state a binding
passes through, dropping is what happens to `Own` at a drop point. Closure capture
analysis stays where it is (`collect_captures`, lowering time **[fact]**), but its
output feeds owncheck (captures are moves of `𝒜` into the env allocation).

---

## E. Subsystem designs

### E.1 Closures (Part 4)

**Representation.** `Closure = { code ptr, env ptr }` **[fact, unchanged]**, but the
env becomes an *owning allocation* and the closure value becomes **move-only** (today
closure values are Copy — `ownership.rs:6-8`; that treatment is retired).

**Capture rules.**

- *Capture by move* (already the rule for non-Copy captures **[fact]**): `(fn [] x)`
  moves `x`; `x` is `Dead` afterwards; the env owns the captured allocation; env drop
  runs the captured value's glue. Scalars and Static strings capture by copy (no
  tracking), exactly today.
- *Env allocation identity*: the `Closure` expression mints `α_env`; the binding owns
  `Own({α_env} ∪ 𝒜_captures)`. Closure call *borrows* the binding (calling twice is
  legal — a read), so today's ergonomics survive; the env drops when the *binding*
  drops.
- *Capture by borrow*: **not in this design's v1.** Borrowed captures require regions
  (§E.5) because a closure is a value that can escape any lexical scope. When regions
  land, `&`-captures are permitted under the rule *a closure may capture `x` by borrow
  only if x's region encloses every use of the closure value* — enforced by the same
  lexical-region machinery, defaulting to reject.
- *Recursive closures*: a closure value cannot reference itself (the binding does not
  exist while its initializer is checked; adding it would need a cycle through the env,
  i.e. an arena or double-boxing). Named `defn` recursion is the supported form
  **[fact: defn recursion is static]**. Documented limitation, revisit only with a
  concrete kernel need.

**Drop glue.** `@drop__lambda_N`: run glue on captures in reverse slot order, free the
env. Generated per lambda alongside today's out-of-line bodies **[fact:
Program.lambdas]**.

**Hard cases** (closures returned: env ownership transfers to the caller, callee does
not drop — the escape is an `eff`; closures stored in collections: requires
`Elem::Fn`/ADT `Fn` fields, which don't exist yet **[fact: Elem has no Fn variant]** —
when added, env ownership moves into the collection and the collection's glue calls the
lambda glue; closures called from fused loops: today fused stages borrow captures
**[fact]** — unchanged, borrows don't consume).

### E.2 Persistent HAMT maps (Part 5)

Options evaluated:

| Criterion | A. per-node refcount | B. arena per map | C. static unique/shared proof |
|---|---|---|---|
| runtime overhead | 8B/node + inc/dec traffic | lowest in build phase, but assoc-then-keep-old breaks arena-per-value | zero where provable |
| memory overhead | +1 word/node | arena waste for survivors | none |
| complexity | low (runtime.c + glue) | high; changes persistence semantics or needs epochs | high (whole-program proof, per-node lineage) |
| deterministic | yes — count 0 frees immediately | yes | yes |
| thread safety | non-atomic counts OK while maps are single-threaded values (parallel reduces consume vectors **[fact]**); switch to atomic counts if maps cross threads | n/a | n/a |
| assoc/dissoc | new path nodes inc shared siblings; root swap | requires copying arena (breaks persistence) or epochs | in-place possible when lineage provably unique |
| recursive structures | acyclic DAG — counts terminate (no Rc-cycle hazard) | — | — |
| interaction with ownership | maps stay move-only owning values; drop = unref | maps own arenas — sharing across `assoc` results contradicts one-arena-per-value | identical to today |

**Recommendation: A, with C as a later optimization.** Refcounting is the only option
that preserves persistence semantics exactly, reclaims deterministically, and slots
into the existing drop-glue architecture (map drop = `unref` walk). It does not make
Cato a reference-counted language: *only* HAMT interior nodes are refcounted, maps
remain uniquely-owned move-only values, and the refcount is an implementation detail of
one family's sharing — invisible to source. C (allocation-identity lineage proofs) can
then elide counts for provably-linear lineages (e.g. build chains where the old map is
dead after each `assoc`) as a profile-driven optimization; the semantic is unchanged.
B (arenas) is rejected as a general mechanism because `assoc` results must outlive
their source — the defining property of persistence — which contradicts per-value
arenas; it remains valid for a future explicit `transient` batch API.

### E.3 `get` / `nth` (Part 6)

Semantic change (breaking, pre-1.0):

```text
(get m k)        : (Option V)         absence is a value you can match on
(get m k d)      : V                  explicit default, eagerly evaluated, documented
(nth v i)        : (Option T)         vectors and strings (byte → (Option i64))
(nth v i d)      : T
```

- **Representation**: `Option V` is the existing monomorphized ADT **[fact]** — inline
  `{tag, payload}` for scalar `V` (16 bytes for `Option i64`), boxed when `V` owns
  (the existing owns→boxed rule **[fact]**). `Some`/`None` construction and `match`
  need no new machinery; `(get m k)` lowers to `Ctor`/`Match` over the existing
  `MapGet`/`Get` nodes with the default arm producing `None`.
- **Ownership/drop**: the Option owns its payload; binding it and dropping it runs
  payload glue transitively — a special case of nothing; it is just an ADT.
- **Bounds semantics**: `Some` iff `0 ≤ i < len` (and key present for maps). No new
  panic surface; the explicit-default form is the ergonomic fast path.
- **Performance**: `Option i64` is two registers; a `match` producing a value in both
  arms folds to the same branchless select emitted today **[fact: branchless `get`]**,
  so the safe form costs the same as the current footgun. The 3-arg form is emitted
  identically to today. A later "provably in-bounds" analysis can drop the select; not
  required.
- **Ergonomics**: the common read `(get m k)` now forces a `match`/`if-let`; add
  `(if-let [v (get m k)] …)` (macro exists **[fact: expand.rs]**). This is the loudest
  safety signal the language can ship, and it deletes the eager-vs-lazy default
  evaluation divergence class from the parity suite.
- **Migration**: ship alongside the old behavior under a flag for one slice? No —
  `examples/` is the conformance suite; flip all examples in one TDD slice (they are
  few and mechanical: `get` → 3-arg or match).

### E.4 Typed HIR (Part 8)

- After typing (lowering), a `type_ops` canonicalization pass rewrites every `Bin` →
  `BinW{op, lhs, rhs, ty}` / `BinF` / explicit `ICast`, and every `Cmp` →
  `CmpW{…ty}` with `ty ∈ {widths, f32, f64, bool}` — the nodes already exist **[fact:
  BinW/CmpW/BinF/ICast]**; what changes is that they become the *only* post-lowering
  forms, carrying width + signedness, so neither backend reconstructs semantics.
- **Promotion**: make integer→float promotion *explicit* — `(+ i 1.0)` becomes a
  compile error ("cannot mix i64 and f64 — cast with (f64 i)"), matching the existing
  f32 strictness **[fact: "cannot mix f32 with other widths"]**. This deletes the
  runtime re-derivation and the float-contagion trap in one move. Breaking; pre-1.0.
- **Arithmetic**: wrapping at each width (today's behavior **[fact: wrapping_add /
  u8-wrap examples]**), documented; no UB anywhere. `Div`: today `sdiv`/wrapping
  (LLVM) with INT_MIN/-1 as hardware behavior; pin it: `i64::MIN / -1` → wrapping
  (document) — the interp must mirror exactly, verified by fuzzing (Part 9).
- **Signed/unsigned comparisons**: `CmpW`'s `ty` selects `slt`-family vs `ult`-family
  (already true **[fact: exprs.rs:66-73]**); the interp's unsigned branch (shipped in
  the recent fix) becomes the *only* path.
- **Constants**: literals stay `ConstI64`/`ConstF64`; `type_ops` inserts `ICast` when a
  narrow-width binding consumes a literal (today's `wrap` at cast points **[fact]**,
  now uniform).
- **Interpreter**: evaluates from the node's `ty` alone; `ty_of_val` is deleted; the
  `Value` enum's `Int(i64)` stays as the storage (bit patterns) — semantics come from
  the node, not the value. Architecture: `source → type_ops → Typed HIR → {interp,
  LLVM}` as the prompt requires.

### E.5 Borrow regions (Part 7)

Deliberately less than Rust, because Cato's mutation surface is nearly empty (in-place
`conj` is the only mutator **[fact]**; no `set!`, no interior mutability — S1 §6
non-goals). Rust's regions exist to make *mutation through references* safe across
arbitrary aliasing; Cato doesn't need that machinery to get 90% of the safety.

**v1 model: lexically scoped, non-storable borrows with inferred lifetimes.**

- A `&x` / `&!x` expression is a first-class *borrow value* usable only as: a call
  argument, a `let`-bound transient, or a `match` scrutinee. Borrows **cannot** be
  stored in ADTs, vectors, maps, or returned (type rule: no type mentions a region, so
  no type can hold one — nothing to infer except "does not escape", which the
  occurrence check settles syntactically).
- The region of `&x` is the enclosing `let`/call statement: the borrowed binding is
  `Borrowed` (immovable, non-droppable) until the end of that lexical region; two
  exclusive borrows may not overlap (today's same-call rule **[fact]**, generalized to
  the lexical region).
- `&!` grants the in-place path: `(conj (&! v) x)` mutates through the borrow *without
  consuming* `v` — the first legal through-borrow mutation, and the payoff for regions.
  (Today conj moves its source **[fact]**; that stays the default, borrow-conj is the
  explicit opt-in.)
- **Escape prevention** is by construction: borrows are unstatable, so `borrow →
  closure → escape` and `borrow → collection → escape` cannot be written; `borrow →
  owner dropped` cannot happen because the region ends before the scope that could drop
  the owner.
- Returning references: not supported (values return by move). If a future need appears
  (e.g. zero-copy `subs` views), that is the point to introduce real region variables —
  not before.

### E.6 Lifting the remaining conservatism (merge/escape buckets)

The v1 model leaves two leak buckets; both have known lifts, sequenced after the main
migration:

- **Control-flow merges** (`if`/`match` producing owning values): emit *per-arm scratch
  drops* — each arm, after computing its result, drops its own scratch allocation when
  it is not the selected one; the selected result's allocation flows to the merge.
  Requires arm-local stash flushing (the `stash` in §C.3). Soundness: per-arm linear
  histories.
- **Escapes through unknown callees** (calls to `Fn`-typed values whose callee is not
  statically known): today any call result is conservatively fresh **[fact:
  fresh_owning_value]**; fine — the risky direction is *arguments*: owned args transfer
  to the callee by rule, so unknown callees are already exactly as safe as known ones.
  The remaining bucket is only closures capturing-and-then-releasing — resolved by E.1.

### E.7 Differential fuzzing (Part 9)

New crate `crates/cato-fuzz` (std-only, honoring the zero-dep rule **[fact:
ROADMAP]**), two tiers:

- **Tier 1 (with typed HIR, M1):** generator over a small grammar of *value
  expressions*: all integer widths, signed/unsigned corners (top-bit values via
  `(u64 -1)`-style casts), wrapping arithmetic, comparisons, casts, `Option`/ADTs,
  vectors/maps/sets, closures, moves (programs that must be *rejected* — parity of
  acceptance is checked too: both backends must reject the same programs).
- **Tier 2 (after M4/M5):** add borrows, HAMT-heavy assoc chains, `loop`.

Mechanics: xorshift PRNG with logged seeds (deterministic reproduction); each case is
compiled once, run under both backends (`cato run` vs `--emit-ir` + clang), compare
stdout + exit code; classify failures: `compile-error-mismatch | interp-crash |
native-crash | timeout | output-mismatch`, then delta-debug the AST to a minimal
repro, and on confirmation append it to `examples/fuzz_corpus/` (regression corpus,
wired into the parity test list). CI: N seconds of fuzzing per push, full budget
nightly. This converts the golden-example parity gate into a conformance jury —
exactly the gap the CRITIQUE identified **[fact: known divergences were found by hand,
not by the gate]**.

### E.8 Macros (Part 10)

Deferred past this design, with preconditions recorded now: expansion is a *separate
phase before typing* (today's fixed expand.rs **[fact]** already is); a future user
macro system must specify: hygiene (gensym renaming in the expander, no capture of user
identifiers), expansion depth budget with a diagnostic, recursion policy (no
macro-defining-macros in v1), error spans (macro-definition site + use site), and
interaction with typing (macros expand to surface syntax; they never bypass
owncheck/typing). Until then, the docs claim "a fixed macro set", not Lisp macros
**[fact: already reworded in the language-design doc]**.

---

## F. Migration plan

Ordering follows the prompt's priority list with one evidence-based adjustment: the
fuzz *harness* lands with M1 rather than M7, because M1 itself changes numeric
semantics everywhere and the golden examples demonstrably cannot catch width corners
**[fact: the u64 divergence survived the suite until hand testing]**. Every phase ends
green: full suite, ASAN gates, byte-identical parity on `examples/`.

### M0 — Groundwork (no language change)
- Prereqs: none.
- Changes: introduce leak-bucket counting in `annotate_frees` (pure reporting:
  `--dump-mem`); add `examples/fuzz_corpus/` plumbing; pin current parity snapshot.
- Invariant: no behavior change; buckets reported per defn.
- Regression risk: none (reporting only).

### M1 — Typed HIR + Tier-1 fuzzing (Parts 8, 9)
- Prereqs: M0.
- Changes: `type_ops` canonicalization pass; delete interp reconstruction
  (`ty_of_val`, float-when-either); make int↔float mixing a compile error; update
  examples that rely on silent promotion; `cato-fuzz` Tier 1 in CI.
- Modules: `hir/lower.rs` (emit width-carrying nodes directly where trivial),
  new `hir/type_ops.rs`, `interp/lib.rs`, `crates/cato-fuzz/`.
- Tests: existing widths/widths2 parity; new fuzz corpus; negative test for mixed
  arithmetic.
- Invariants: post-`type_ops`, no `Bin`/`Cmp` nodes survive; interp reads width only
  from nodes.
- Regressions: programs relying on silent i64→f64 promotion break (compile error with
  fix-it); acceptable pre-1.0.

### M2 — Ownership-driven drops (Parts 1–3)
- Prereqs: M1 (typed HIR stabilizes the IR owncheck walks).
- Changes: `owncheck v2` with allocation sets and the §C rules (VAR/LET/MOVE/CONJ/
  CALL merges); `assign_drops` replaces `annotate_frees` + `recompute_frees` outputs;
  per-binding env slots in the LLVM backend (shadowed bindings stop sharing slots);
  `conj` becomes a consuming op in the checker (source `Dead` — today it already marks
  moved **[fact]**, so the observable rule is unchanged; what changes is that the free
  entry *transfers*); str provenance unification (drop = free-or-noop by `Heap/Static`,
  deleting `fresh_owning_value`'s str special-cases); clone elision retires into the
  transfer rule.
- Modules: `ownership.rs` (majority), `opt/mod.rs`, `heap_vec.rs: emit_let_frees`
  (allocation-keyed), `adt.rs` glue unchanged.
- Tests: transfer tests (`let [y x]` no longer leaks), conj-chain tests, ASAN on all
  examples with `--no-opt` and `--opt`, new negative tests (double explicit drop).
- Invariants: G1–G5 (§H). The migration invariant: **a free may be removed or
  transferred, never duplicated** — any owncheck uncertainty lands in a bucket.
- Regressions: strictly fewer leaks; a subtle risk is *double-slot* codegen for
  shadowing — covered by the parity suite (byte-identical IR changes here, so gate on
  output parity, not IR identity, for this phase).

### M3 — Closure ownership (Part 4)
- Prereqs: M2 (env is just another owned allocation).
- Changes: closure values move-only; `@drop__lambda_N` glue; capture = move into env
  (checker wires `collect_captures` output through §C rules); ClosureCall = borrow;
  examples updated if any rely on Copy closures (grep says examples pass closures
  lexically — low exposure).
- Modules: `ownership.rs`, `closures.rs` (emit glue + frees), `adt.rs` (glue
  registration), interp untouched.
- Tests: closure created in a loop → flat RSS under ASAN (the acceptance test);
  use-after-move of closure values rejected.
- Invariants: every closure env has exactly one destruction path.
- Regressions: a program that relied on closures being Copy (passing the same closure
  by value twice to an owned param) now needs `(clone f)` or a borrow — rare; diagnostics
  guide the fix.

### M4 — HAMT refcounting (Part 5)
- Prereqs: M2 (drop glue is allocation-driven).
- Changes: 8-byte count in HAMT node header; `cato_hamt_assoc`/`get` inc shared
  children; map drop = `unref` walk in glue; flat path unchanged (still single-free);
  emit_let_frees' "flat-only" special case **[fact]** is deleted — the glue decides.
- Modules: `runtime/runtime.c` (hamt helpers), `heap_map.rs`, `heap_vec.rs`, `adt.rs`.
- Tests: `examples/hamt.cato` under ASAN with a leak checker (currently leaks by
  contract — this phase *flips that contract*); share-then-drop-both orderings.
- Invariants: nodes free exactly when the last referencing map's glue runs;
  counts are exact (no cycles — HAMT is a DAG).
- Regressions: perf on assoc-heavy code (measure; the count ops are cache-local).

### M5 — `Option` get/nth (Part 6)
- Prereqs: M1 (typed HIR), independent of M2–M4 otherwise; placed here so its examples
  churn happens once, after the memory-model churn.
- Changes: `MapGet`/`Get`/`StrNth` two-arg forms return `Option`; 3-arg keep today's
  shape; examples + docs flipped; fuzz Tier 1 extended with Option-heavy programs.
- Tests: parity on new forms; negative tests (no silent 0).
- Regressions: every `get` call site in examples updates (mechanical).

### M6 — Lexical borrows (Part 7)
- Prereqs: M2 (Borrowed state exists).
- Changes: `&x`/`&!x` as borrow *expressions* (today sigils are annotation-only
  **[fact]**); region = enclosing statement; `(&! conj)` in-place path; storable-borrow
  rejection rule; diagnostics for region violations.
- Tests: region-escape negatives; borrow-conj parity; ASAN.
- Regressions: minimal (new syntax; nothing existing changes).

### M7 — Fuzz expansion (Part 9, Tier 2)
- Prereqs: M4–M6 give the fuzzer its full target surface.
- Changes: grammar extensions; nightly budget; corpus promotion workflow.

### M8 — Macros design-only (Part 10)
- A one-page spec addendum (§E.8 constraints), no implementation, revisited after S9.

---

## G. Safety invariants

Each is checkable by the compiler or the test suite:

```text
G1  No owned allocation is dropped more than once on any path.
    (owncheck: each α appears in exactly one drop list per linear history;
    ASAN over examples/ with opt on and off is the regression net.)

G2  No moved value can be used. (E301, enforced pre- and post-optimization —
    already true [fact], preserved.)

G3  No borrow can outlive its owner. (v1: borrows are lexically scoped and
    unstorable — the region ends before the owner's scope can end.)

G4  Every implicit drop happens only while ownership is held.
    (drop lists are generated exclusively from Own states at the drop point;
    a Dead/Leaked binding never appears in a drop list.)

G5  Every closure environment has a deterministic destruction path. (M3.)

G6  Shared HAMT nodes are destroyed exactly when the last referencing map
    drops. (M4; test: interleaved assoc/dissoc/drop orderings under ASAN.)

G7  Unknown ownership never frees. (Leaked(b) is the only representation of
    uncertainty, it never emits a free, and it is reported, counted, and
    tracked to zero — the old "anything uncertain leaks" rule, made explicit
    and measurable.)

G8  Interpreter and LLVM implement identical typed-HIR semantics.
    (M1 makes width/signedness a property of nodes; the fuzz corpus is the
    continuous check; parity on examples/ is the floor, not the ceiling.)

G9  The optimizer never invents or destroys a free. (recompute_drops after
    every rewrite round; verify_ownership is the loud safety net — pattern
    already shipped [fact: opt/mod.rs].)

G10 Explicit `drop` and implicit drop cannot race each other: both consume;
    the second consumer is a compile error.
```

---

## H. Final recommendation

Build, in order: **M1 typed HIR (+ Tier-1 fuzzing) → M2 ownership-driven drops → M3
closure envs → M4 HAMT refcounts → M5 Option collections → M6 lexical borrows → M7
fuzz expansion**, with macros deferred behind a design note (M8).

The resulting architecture, in one paragraph:

```text
Cato is a no-GC language whose memory strategy is compiler-derived:

  - Ownership is affine and allocation-precise: every heap allocation has
    exactly one owning binding at every program point, and moves, borrows,
    consumes, and drops are transitions of one state machine — not
    independent heuristics.

  - Drop is the default consequence of ownership at scope exit, generated
    as per-type glue; explicit `drop` and `clone` are the user's two
    overrides; there are no user destructors.

  - Sharing is explicit and rare: Copy aliasing for scalars and static
    strings, and refcounted nodes for exactly one family (persistent HAMT
    maps) whose semantics genuinely require it — deterministic, bounded,
    and invisible at the source level.

  - What the compiler cannot prove is not silently leaked but named,
    counted (Leaked buckets), and driven to zero — with a diagnostic-first
    culture: unsupported shapes are rejected or bucketed, never mis-freed.

  - One typed HIR feeds both backends; a differential fuzzer, not golden
    examples alone, is the conformance jury.

  - No tracing collector, no pauses, no managed runtime: reclamation is
    lexical, deterministic, and explains itself.
```

This keeps every non-negotiable: no GC (G1–G10 are all compile-time or
deterministic-runtime properties), zero-cost when ownership is statically obvious
(transfer and drop lists compile to the same `free` calls the shape heuristic emits
today, minus the leaks), and it retires the one intellectual debt the current model
carries — that *freshness of shape*, not *ownership*, decides destruction.
