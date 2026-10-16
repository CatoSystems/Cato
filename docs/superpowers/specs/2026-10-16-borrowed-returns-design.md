# Borrowed Returns (`&T`) for the Cato Kernel — Design

Date: 2026-10-16
Status: **Proposed.** Implementation plan to follow via `writing-plans`.

## 1. Summary

The self-hosting kernel's peak footprint while compiling itself is ~8–9 GB and
grows each bootstrap stage; the fixpoint goal is that every stage's footprint
approximates its predecessor's. Profiling shows the peak is **not** a leak: it is
hundreds of millions of deep clones of persistent HIR/ADT structures. This spec
introduces one small language feature — **borrowed returns** (`&T` return
annotation) — so that field-projecting accessors can return a view instead of a
deep copy, and read-only traversals can walk borrowed children without cloning.

Codegen is unaffected: boxed ADTs are already pointers, so `&T` and `T` are
ABI-identical. The feature is entirely **parse + ownership marking**.

A follow-on, separate change **(C)** de-functionalizes any hot structures that
remain after this change; it is out of scope here except as a named next step.

## 2. Evidence (root cause)

Instrumenting the generated `malloc` calls with caller attribution while cato2
builds the kernel yields (cumulative over one build):

| allocating site | calls |
|---|---|
| `clone__HExpr` | **604 M** |
| `clone__TyList` | **619 M** |
| `clone__HExprs` | 415 M |
| `clone__Sids` | 347 M |
| `clone__VList` | 227 M |
| `clone__AdtTab` / `clone__HArms` / `clone__PList` | 157 M / 79 M / 73 M |

That is ~100 GB churned to reach a ~9 GB peak (91 M live MALLOC_SMALL nodes at
the 3.8 GB sample point). The top *source* callers are:

- **Projecting accessors** that take a value by value and return a field:
  `vlist_at` (1.9 M), `find_variant_2` (1.8 M), `adts_at` (136 800), `esc_get`,
  `sigs_at`, `tys_idx`, `fs_sum`, `fs_pay_vi`, `variant_index`, `name_of`,
  `hv_a`/`hv_b`/`hv_c`, `hargs_of`, `arms_of`. Because Cato forbids `&T` return
  types, each must `clone` the field it returns — and those fields (`AdtInfo`,
  `Varnt`, `HExprs`, `TyList`, `Sids`) **embed linked lists**, so one "read" deep
  copies a whole chain.
- **Read-only tree walks** that take `HExpr` by value and reach children through
  those same projecting accessors (`b_count_vars`, `b_count_binds`,
  `b_tail_escape`, `contains_recur`, `b_kids`, …): one clone per node visited.

The seed's Rust backend does not exhibit this because it returns `&T` / uses
`Vec` + indices. The kernel's Cato source was written in a functional
copy-on-write style and had to clone to stand in for missing references.

## 3. Design

### 3.1 Syntax

A return annotation may carry the borrow sigil, reusing the existing
`borrow-sigil` / `split_borrow` machinery already used on parameters:

```
(defn hv-b      [e :- &HExpr] :- &HExpr   (match e (HIf [c t f ty] t) (_ e)))
(defn adts-at   [ts :- &AdtTab i :- i64] :- &AdtInfo
  (match ts (ATCons [h t] (if (= i 0) h (adts-at t (- i 1)))) (ATNil [] (adts-nil))))
```

The `&` is **stripped** at parse time. The declared `Ty` is unchanged (still
`HExpr` / `AdtInfo`); a new boolean is recorded on the defn:

- seed: `Defn.ret_borrowed: bool` (and `RawDefn.ret_borrowed`).
- kernel: `Defn.ret-borrowed :- i64` (and `RawDefn`).

No new `Ty` variant is introduced, so type equality, monomorphization, and
mangling are unaffected.

**Constraint:** only **boxed** types may be borrowed returns (recursive ADTs:
`HExpr`, `HExprs`, `Binds`, `HArms`, `AdtInfo`, `Varnt`, `VList`, `TyList`,
`Sids`, `AdtTab`, `Caps`, …). A `&scalar` return (e.g. `&i64`) is a compile
error — a borrow of a scalar has no stable address in this model. This mirrors
the existing fact analysis (`boxed = on a cycle in the ADT-field graph`).

### 3.2 Soundness rule (no lifetimes)

A `&T`-returning function's tail expression must evaluate to a **non-owned**
value:

- Borrowedness only ever originates from `&` (Shared) parameters. `&` params are
  immutable and can never be moved or freed inside the callee, so any value
  derived from them stays valid for the caller's entire enclosing scope. A
  returned borrow is therefore valid for the whole caller scope — no lifetime
  tracking or region inference is required.
- Checker enforcement: when checking the body of a `&T` function, if the tail
  expression's value is **owned** (owns == true) the program is rejected with
  `E304: borrowed return must borrow from a parameter (found an owned value;
  clone or return a borrowed field)`. This rejects fresh allocations, owned
  locals, and `(clone x)` returns, without provenance tracking.
- A borrow may be chained: `f :- &A` calls `g :- &B` (where `g` borrows an `&`
  param of `f`) and returns it — still rooted at an `&` param of `f`. Sound.
- Borrows of module-level constants/globals are also sound (immortal) and are
  permitted by the same "non-owned" test.

This is deliberately the weakest sufficient rule: it forbids only what is
unsound, and every accepted program is memory-safe with respect to the borrow.

### 3.3 Ownership marking (the behavioral change)

A call to a `ret_borrowed` callee produces a **borrowed value**:

- **Non-owning:** binding it (`let`/loop/match binder) sets `owns = false`.
- **Non-droppable:** it is never added to a binding's or parameter's `frees`
  set, and it is never emitted as a `drop`.
- **Non-movable:** passing it to an `Owned` parameter is an ownership error
  (`E303`, "move of a borrowed value", as for `&` params today); the caller must
  `clone` explicitly to obtain an owned copy.
- As an argument to a `Shared` (`&`) parameter it is passed through unchanged.

Requires a name → `ret_borrowed` lookup in both engines. This mirrors the
existing `dparams` table in the kernel (`DParams`/`dparams-get`) and the seed's
`sigs: HashMap<String, Vec<Borrow>>`: extend the signature table with the
callee's return-borrowed flag.

Callee side: binders from a borrowed `match` (matching an `&T` scrutinee) are
already `owns = false`, so returning such a binder naturally satisfies §3.2 with
no extra work.

### 3.4 Codegen

**None.** In both engines a boxed ADT value is an LLVM pointer; `&T` and `T`
have the same representation, so `lower`/`emit` (kernel) and the seed's lowering
and LLVM backend are untouched. Only the ownership state changes.

## 4. Implementation touch points

Both compilers must implement the feature identically, because the seed builds
`cato1h` from the kernel source and the kernel builds every later stage.

### Seed (`crates/`)
- `cato-core/src/hir/parse.rs` — `parse_defn` / `RawDefn` construction: detect a
  leading `&` on the return annotation via `split_borrow`, strip it, set
  `ret_borrowed`; reject `&` on a non-boxed return type.
- `cato-core/src/hir/mod.rs` — add `Defn.ret_borrowed`, `RawDefn.ret_borrowed`.
- `cato-core/src/hir/ownership.rs` — extend `kinds_of`/sigs with the callee
  return flag; in `HExpr::Call` mark the result borrowed; add the tail check for
  `&T` bodies (§3.2); ensure borrowed bindings get `owns=false` and are never
  freed.
- `cato-core/src/hir/build.rs`, `generics.rs` — carry `ret_borrowed` through
  monomorphization/specialization signature construction.
- `cato-core/src/diagnostics.rs` — register `E304` (borrowed return requires a
  borrowed value) alongside `E301`/`E303`.
- (No change) lowering and `cato-backend-llvm`.

### Kernel (`selfhost/cato/`)
- `hir/forms.cato` — `RawDefn` and `Defn` gain `ret-borrowed`; `parse-defn-ret`
  detects `&` (reuse `borrow-sigil`) and sets it; reject non-boxed `&` returns.
- `hir/expand.cato` — thread the flag when building `Defn`s.
- `hir/moveck.cato` — build a name → ret-borrowed table (like `DParams`); in the
  call handling mark the result borrowed; add the tail check for `&T` bodies;
  ensure borrowed bindings are `owns=0` and never freed/moved.
- (No change) `hir/lower.cato`, `emit/*`.

## 5. Application plan (where the win comes from)

1. **Feature** — implement §3 end-to-end in seed and kernel; keep the kernel
   source unchanged (no behavior change yet). Verify the bootstrap still passes
   and the fixpoint (`cato2.ll == cato3.ll`) holds.
2. **Accessors** — convert projecting accessors to borrowed returns:
   `hv-a`/`hv-b`/`hv-c`, `hargs-of`, `arms-of`, `name-of`, `vlist-at`,
   `adts-at`, `sigs-at`, `find-variant-2`, `esc-get`, `tys-idx`, `fs-sum`,
   `fs-pay-vi`, `variant-index`, and the `hes-*`/`habs-*`/`hargs-*` query
   helpers. Update call sites to drop the now-unnecessary `(clone …)`.
3. **Traversals** — rewrite read-only walks (`b-count-vars`, `b-count-binds`,
   `b-tail-escape`, `b-tail-escape-last`/`arms`, `contains-recur`, `b-kids`,
   `b-shadowed-of`, `tys-depth`, `defns-depth`, `adts-depth`, `sigs-depth`,
   `vlist-depth`, `plist-depth`, `hes-depth`) to take `&HExpr`/`&Exprs` and
   thread borrows — no clones.
4. **Measure** at each step with the malloc-caller harness (§7) and record the
   peak and the top clones.
5. **Next change (C)** — de-functionalize the structures still hot afterward.

## 6. Non-goals

- General lifetime/region inference. The rule in §3.2 is intentionally weaker.
- Borrowed **fields/structs** as first-class returned values of non-boxed type.
- Mutable/exclusive borrows (`&!` remains removed).
- Changing `clone`/`drop` semantics, HAMT refcounting, or collection layouts.

## 7. Testing & acceptance

**Correctness fixtures** (interp + kernel_emit + the `examples/` suite):
- borrowed return accepted and used without a clone;
- owned return from a `&T` function rejected (`E304`);
- borrowed result is never freed (IR assertion: no `drop__`/`free` of the
  borrowed binding);
- passing a borrowed value to an `Owned` parameter rejected (`E303`),
  accepted when passed to a `Shared` parameter;
- chained borrows (`f :- &A` via `g :- &B`);
- `&` on a non-boxed return rejected.
- Existing `examples/errors/*` and the full `examples/` suite stay green.

**Memory acceptance** (the goal):
- Re-run the malloc-caller harness and the peak-RSS monitor while cato2 builds
  the kernel. Success = a material drop in peak (target: well under the current
  ~9 GB; ideally toward the seed's footprint) and a corresponding drop in the
  top `clone__*` counts, **with**:
- `cato2.ll == cato3.ll` byte-identical (fixpoint preserved),
- `cato2`/`cato3`/`cato4` run t1–t6 and the full self-host chain,
- the self-host chain is ASAN-clean (`clang -O0 -g -fsanitize=address …`) with
  no over-free/UAF.

## 8. Risks & mitigations

- **Dual implementation drift** (seed vs kernel). Mitigate by implementing the
  seed first and validating it with fixtures, then mirroring exactly in the
  kernel; the fixpoint check is the cross-check that they agree.
- **Unsound mis-marking** (freeing a borrowed value, or returning an owned one).
  Mitigated by the explicit tail check and by the "borrowed ⇒ never freed" IR
  assertion, plus ASAN on the self-host chain.
- **`&` on non-boxed returns** producing meaningless pointers. Mitigated by the
  explicit rejection using the existing boxed fact analysis.
- **Scope creep into a full borrow checker.** Explicitly non-goal (§6).
