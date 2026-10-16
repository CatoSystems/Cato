# Borrowed Returns (`&T`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a borrowed return annotation (`&T`) to Cato so field-projecting accessors and read-only traversals in the self-hosting kernel return/walk borrows instead of deep clones, cutting the ~8–9 GB peak footprint of the kernel compiling itself.

**Architecture:** `&T` is a *parse + ownership-marking* feature only. The `&` sigil is stripped at parse time; the declared `Ty` is unchanged and a boolean (`ret_borrowed` / `ret-borrowed`) is recorded on the defn. A call to a borrowed-returning callee yields a non-owning, non-droppable, non-movable value. No codegen change: boxed ADTs are already pointers, so `&T` and `T` are ABI-identical. Must be implemented in **both** compilers — the Rust seed (builds `cato1h` from kernel source) and the Cato kernel (builds every later stage).

**Tech Stack:** Rust (seed: `crates/cato-core`, `crates/cato-cli`), Cato self-host source (`selfhost/cato/`), LLVM IR via `clang`, C runtime (`runtime/runtime.c`).

**Spec:** `docs/superpowers/specs/2026-10-16-borrowed-returns-design.md`

## Global Constraints

- No new `Ty` variant. `&T` return is a flag on the defn; the type is stored unchanged.
- Only **boxed** types may be `&T` returns (recursive ADTs: `HExpr`, `HExprs`, `Binds`, `HArms`, `AdtInfo`, `Varnt`, `VList`, `TyList`, `Sids`, `AdtTab`, `Caps`, …). `&i64`/`&str`/`&vec`/`&Box` (non-recursive) returns are a compile error. Use the existing boxed-fact analysis (`facts.boxed` seed; `facts-boxed-at` kernel).
- The borrowed return type is written as a **symbol** (`:- &HExpr`); `&` on a list-form annotation (`&(Tree a)`) is a compile error. This matches the existing `&`-param support (symbol annotations only).
- A `&T` body's tail value must be **non-owned** (`owns == false`); otherwise reject with new code `E304`. Register `E304` in `crates/cato-core/src/diagnostics.rs`.
- Passing a borrowed value to an `Owned` parameter is `E303` ("move of a borrowed value"), exactly as for `&` params today.
- Codegen (`hir/lower.cato`, `emit/*`, seed lowering + LLVM backend) must **not** change.
- Both compilers must accept/reject the same programs. After every kernel-source edit: regenerate `selfhost/cato.cato` via `python3 tools/concat_kernel.py`, and verify per-line paren balance.
- Kernel CLI is positional only: `/tmp/catoX build <src> <out> [rt]`. Seed CLI supports `--emit-ll <path>`.
- Green gate every task: the relevant `cargo test` suites pass; for kernel tasks, `cato2`/`cato3` run `/tmp/t1.cato`–`/tmp/t6.cato` and the self-host chain, with `cato2.ll == cato3.ll` byte-identical.

## Review Focus

Uncovered inputs/conditions most likely to bite a user, each pinned by a task test:

1. **Owned tail in a `&T` body** (e.g. `(clone x)`, a local `let`, a fresh constructor) — must be rejected `E304`, never silently emitted (a returned owned value could be freed by the caller while aliased). → Task 2 (seed) + Task 5 (kernel).
2. **Borrowed result bound then passed to an `Owned` param** — must be `E303`, not a use-after-free. → Task 2 + Task 3.
3. **`&` on a non-boxed return** (`&i64`, `&str`, `&vec`) — must be rejected with a clear error, not emit a pointer to a temporary. → Task 1 + Task 4.
4. **Chained borrows** (`f :- &A` returns `g`'s `&B` result) — must remain valid, not freed at the inner call boundary. → Task 2 + Task 3.
5. **Seed/kernel divergence** — a form accepted by one compiler and rejected by the other breaks the bootstrap; the fixpoint check is the detector. → Task 6.

---

## File Structure

**Seed (`crates/`)**
- `cato-core/src/hir/mod.rs` — `Defn`, `RawDefn`: add `ret_borrowed: bool`.
- `cato-core/src/hir/parse.rs` — `parse_raw`, `parse_extern`: detect `&` on the return annotation (reuse `split_borrow`), reject non-boxed.
- `cato-core/src/hir/ownership.rs` — `kinds_of`/sigs table carries the callee return flag; `Call` result marked borrowed; `E304` tail check.
- `cato-core/src/diagnostics.rs` — register `E304`.
- `cato-core/src/hir/build.rs`, `generics.rs` — carry `ret_borrowed` through monomorphization.
- `cato-core/tests/` — new fixtures/tests.

**Kernel (`selfhost/cato/`)**
- `hir/forms.cato` — `RawDefn`, `Defn`: add `ret-borrowed`; `parse-defn-ret` detects `&`; reject non-boxed.
- `hir/expand.cato` — thread `ret-borrowed` when building `Defn`s.
- `hir/moveck.cato` — name → ret-borrowed table (like `DParams`); call-result marking; `E304` tail check.
- `selfhost/cato.cato` — regenerated concat.

**Application (kernel source only)**
- `hir/hexpr.cato`, `hir/types.cato`, `hir/forms.cato`, `emit/state.cato`, `hir/expand.cato`, `hir/lower.cato` — convert projecting accessors and read-only traversals.

---

### Task 1: Seed — `ret_borrowed` data model + parse

**Files:**
- Modify: `crates/cato-core/src/hir/mod.rs` (`Defn` ~434, `RawDefn` ~447)
- Modify: `crates/cato-core/src/hir/parse.rs` (`parse_raw` 280, `parse_extern` 20)
- Test: `crates/cato-core/tests/borrowed_returns.rs` (create)

**Interfaces:**
- Consumes: `split_borrow(s: &str) -> Result<(Borrow, &str), String>` (parse.rs:467); `TypeCx` boxed facts.
- Produces: `Defn.ret_borrowed: bool`, `RawDefn.ret_borrowed: bool`; parse accepts `(defn f [...] :- &T body)` and rejects `&` on a non-boxed `T`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/cato-core/tests/borrowed_returns.rs
use cato_core::compile_to_program;

#[test]
fn borrowed_return_parses() {
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
"#;
    assert!(compile_to_program(src).is_ok());
}

#[test]
fn borrowed_return_list_form_rejected() {
    // `&(Tree i64)` reads as a standalone `&` + list -> reject with guidance.
    let src = r#"
(deftype (Tree a) (Node [v :- a l :- (Tree a) r :- (Tree a)]) (Leaf))
(defn leaf [t :- (Tree i64)] :- &(Tree i64) (Leaf))
(defn main [] :- i64 0)
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("borrowed return"), "got: {e}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cato-core --test borrowed_returns`
Expected: FAIL (compile error: `ret_borrowed` field / unsupported `&` return).

- [ ] **Step 3: Add the fields and parse logic**

Add `pub ret_borrowed: bool` to `Defn` and `pub(crate) ret_borrowed: bool` to
`RawDefn`. In `parse_raw`, when the return annotation is present, inspect
`items[body_start + 1]`:

- `Expr::LitSym(s)` where `split_borrow(s)` is `Shared` with a non-empty rest →
  strip and set `ret_borrowed = true`, parse the rest with `parse_ty_sym`.
- `Expr::LitSym("&")` (standalone) or any non-symbol annotation carrying a
  sigil → error mentioning "borrowed return" (the list-form marker).
- otherwise → owned (existing path).

Only form/sigil is checked here; the boxed check happens in Task 2 where ADT
facts exist. In `parse_extern`, set `ret_borrowed: false`. Update all
`RawDefn { .. }`/`Defn { .. }` constructions to compile.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p cato-core --test borrowed_returns`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cato-core/src/hir/mod.rs crates/cato-core/src/hir/parse.rs crates/cato-core/tests/borrowed_returns.rs
git commit -m "feat(seed): parse borrowed return annotation (ret_borrowed)"
```

---

### Task 2: Seed — ownership marking + `E304` tail check

**Files:**
- Modify: `crates/cato-core/src/hir/ownership.rs` (`kinds_of` 299, `Call` 588-606, `plan_drops`/wrapper 116-207)
- Modify: `crates/cato-core/src/hir/build.rs` (boxed-fact validation; `compute_boxed` 688 in `mod.rs`)
- Modify: `crates/cato-core/src/diagnostics.rs` (register `E304`)
- Test: `crates/cato-core/tests/borrowed_returns.rs` (extend)

**Interfaces:**
- Consumes: `Defn.ret_borrowed` (Task 1); `compute_boxed(adts: &[AdtDef]) -> Vec<bool>` (mod.rs:688).
- Produces: calls to a borrowed-returning callee bind `owns=false` and are never added to a `frees` set; `E304` for an owned tail; a clear error for a borrowed return of a non-boxed type.

- [ ] **Step 1: Write the failing tests**

`crates/cato-core/tests/borrowed_returns.rs` (diagnostics-level cases):

```rust
use cato_core::compile_to_program;

#[test]
fn borrowed_return_body_rejects_owned_tail() {
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn bad [n :- &N] :- &N (Node n n))
(defn main [] :- i64 0)
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("E304"), "got: {e}");
}

#[test]
fn borrowed_return_non_boxed_rejected() {
    let i64_case = "(defn f [x :- &i64] :- &i64 x)\n(defn main [] :- i64 0)\n";
    let adt_case = "(deftype Box (B [v :- i64]))\n(defn f [x :- &Box] :- &Box x)\n(defn main [] :- i64 0)\n";
    for src in [i64_case, adt_case] {
        let e = compile_to_program(src).unwrap_err().to_string();
        assert!(e.contains("borrowed return"), "got: {e}");
    }
}

#[test]
fn borrowed_result_to_owned_param_is_e303() {
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn take [n :- N] :- i64 0)
(defn main [] :- i64 (take (get-l (Node (Leaf) (Leaf)))))
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("E303"), "got: {e}");
}
```

`crates/cato-backend-llvm/tests/codegen_test.rs` (IR-level case; use its existing `emit` helper):

```rust
#[test]
fn borrowed_result_not_freed() {
    let ok = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
"#;
    let ir = emit(ok);
    assert!(!ir.contains("drop__N"), "borrowed binding must not be freed:\n{ir}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-core --test borrowed_returns && cargo test -p cato-backend-llvm --test codegen_test borrowed_result_not_freed`
Expected: FAIL (owned tail accepted / borrowed binding freed / no E303 / non-boxed accepted).

- [ ] **Step 3: Implement the boxed check, marking, and the tail check**

In `build.rs` (where `compute_boxed` runs), reject any `ret_borrowed` defn whose
return type is not boxed, with a message mentioning "borrowed return". Extend the
call-kind table so a callee's `ret_borrowed` is visible at the call site (seed
already has `sigs`/`Borrow`; add the flag). In the `Call` handling, when the
callee is borrowed-returning, mark the destination binding `owns=false` and
exclude it from `frees`. In `plan_drops`, when checking a `&T` defn's body,
require the tail expression's ownership state to be `false`; else emit
diagnostic `E304`. Register `E304` in `diagnostics.rs` with text: *"Borrowed
return requires a borrowed value. A `&T` function may only return a value derived
from its `&` parameters, never an owned or freshly-cloned value."*

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cato-core --test borrowed_returns` then `cargo test -p cato-core`
Expected: PASS (new tests) and no regression.

- [ ] **Step 5: Commit**

```bash
git add crates/cato-core/src/hir/ownership.rs crates/cato-core/src/diagnostics.rs crates/cato-core/tests/borrowed_returns.rs
git commit -m "feat(seed): mark borrowed call results non-owning; E304 tail check"
```

---

### Task 3: Seed — carry `ret_borrowed` through monomorphization + regression fixtures

**Files:**
- Modify: `crates/cato-core/src/hir/build.rs` (133, 145), `crates/cato-core/src/hir/generics.rs` (857, 1033), `crates/cato-core/src/hir/mono.rs` (242)
- Test: `crates/cato-core/tests/borrowed_returns.rs` (extend), `crates/cato-core/tests/diagnostics.rs`

**Interfaces:**
- Consumes: `Defn.ret_borrowed`, `RawDefn.ret_borrowed` (Tasks 1–2).
- Produces: specialized/monomorphized defns preserve the flag; `E304` is discoverable via `--explain`.

- [ ] **Step 1: Write the failing test**

```rust
use cato_core::compile_to_program;

#[test]
fn borrowed_return_survives_specialization() {
    // `f` is generic (param type `a`) but returns a concrete boxed ADT borrow.
    // `H` is recursive (so boxed). Instantiating `f` for `i64` must keep the
    // borrowed-return marking, so `(f x 3)`'s result is not treated as owned.
    let src = r#"
(deftype H (H1 [v :- i64 next :- H]) (H2))
(defn f [x :- &H y :- a] :- &H x)
(defn g [x :- &H] :- &H (f x 3))
(defn main [] :- i64 0)
"#;
    assert!(compile_to_program(src).is_ok());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cato-core --test borrowed_returns borrowed_return_survives_specialization`
Expected: FAIL (flag dropped at instantiation → borrowed result freed / wrong marking).

- [ ] **Step 3: Thread the flag**

In `build.rs`, `generics.rs`, and `mono.rs`, copy `ret_borrowed` into every
`Defn` built from a `RawDefn` (including specialized instances), mirroring how
`borrows` is propagated. (Adding the field makes the compiler flag each missing
site.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cato-core && cargo test -p cato-cli`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cato-core/src/hir/build.rs crates/cato-core/src/hir/generics.rs crates/cato-core/src/hir/mono.rs crates/cato-core/tests/
git commit -m "feat(seed): propagate ret_borrowed through monomorphization; fixtures"
```

---

### Task 4: Kernel — parse `&` return

**Files:**
- Modify: `selfhost/cato/hir/forms.cato` (`RawDefn` 21, `Defn` 34, `parse-defn-ret` 956, `parse-defn-body` 970)
- Regenerate: `selfhost/cato.cato`
- Test: scratch `/tmp/bk1.cato` (create)

**Interfaces:**
- Consumes: `borrow-sigil` (870), `parse-ty` → `PTyRes`, `facts-boxed-at` (102).
- Produces: `RawDefn`/`Defn` gain `ret-borrowed :- i64`; `parse-defn-ret` sets it; non-boxed `&` return rejected.

- [ ] **Step 1: Write the failing test**

```cato
; /tmp/bk1.cato
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
```

Build with the current `cato2`: `/tmp/cato2 build /tmp/bk1.cato /tmp/bk1`
Expected: FAIL — `&` return rejected / `&N` unknown type.

- [ ] **Step 2: Add `ret-borrowed` to `RawDefn`/`Defn` and parse it**

Add a trailing `ret-borrowed :- i64` field to both `deftype`s. In
`parse-defn-ret`, before parsing the type, read the return-type node's text
(`node-str`) and test it with `borrow-sigil`; if the sigil is present, strip the
leading `&` (as `parse-params` does for `ks :- &Asts`) and set
`ret-borrowed = 1`, then parse the bare symbol with `parse-ty-sym-str`. A
standalone `&` (list-form attempt) fails here with the existing unknown-type
error. Only sigil detection happens here; the boxed check is in Task 5. Thread
`ret-borrowed` through `parse-defn-body` into the `RawDefn` constructor. Update
every `RawDefn`/`Defn` constructor for the new arity (grep `(RawDefn ` and
`(Defn `).

- [ ] **Step 3: Regenerate concat and paren-check**

Run: `python3 tools/concat_kernel.py`
Expected: `selfhost/cato.cato` updated; per-line paren balance ok.

- [ ] **Step 4: Rebuild with the seed and run the test**

Run: `./target/debug/cato build selfhost/cato.cato -o /tmp/cato1h` then `/tmp/cato1h build /tmp/bk1.cato /tmp/bk1`
Expected: PASS (builds).

- [ ] **Step 5: Commit**

```bash
git add selfhost/cato/hir/forms.cato selfhost/cato.cato
git commit -m "feat(kernel): parse borrowed return annotation"
```

---

### Task 5: Kernel — moveck marking + `E304` tail check

**Files:**
- Modify: `selfhost/cato/hir/moveck.cato` (`BCtx` 51, `b-plan`, `b-call-move`, `b-arms`)
- Modify: `selfhost/cato/hir/expand.cato` (Defn construction)
- Test: `/tmp/bk2.cato`, `/tmp/bk3.cato` (create)

**Interfaces:**
- Consumes: `RetBorrowed` table pattern (`DParams`/`dparams-get`), `Defn.ret-borrowed` (Task 4), `facts-boxed-at` (102).
- Produces: a name → ret-borrowed table consulted at call sites; borrowed results marked non-owning/non-freed; a non-boxed borrowed return rejected; owned tail in a `&T` body rejected `E304`.

- [ ] **Step 1: Write the failing tests**

```cato
; /tmp/bk2.cato — owned tail must be rejected
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn bad [n :- &N] :- &N (Node n n))
(defn main [] :- i64 0)
```

```cato
; /tmp/bk3.cato — borrowed result must not be freed / not movable
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
```

```cato
; /tmp/bk4.cato — non-boxed borrowed return must be rejected
(deftype Box (B [v :- i64]))
(defn f [x :- &Box] :- &Box x)
(defn main [] :- i64 0)
```

Build `/tmp/bk2.cato` with `cato2`: Expected FAIL (E304).
Build `/tmp/bk3.cato` with `cato2`: Expected FAIL to build (feature not yet in kernel, or a free of the borrow).
Build `/tmp/bk4.cato` with `cato2`: Expected FAIL (non-boxed).

- [ ] **Step 2: Build the ret-borrowed table, wire marking, boxed check, tail check**

Add a table (name → bool), populated from `Defns` like `b-dparams`/`dparams-get`,
carried in `BCtx`. In call handling (`b-call-move` and where a call result is
bound), if the callee is borrowed-returning, mark the destination non-owning and
exclude it from frees. Reject a borrowed-returning defn whose return type is not
boxed (`facts-boxed-at`), with a "borrowed return" message. Add the tail check
for `&T` bodies: error `E304` if the tail value is owned.

- [ ] **Step 3: Regenerate concat, rebuild, verify**

Run: `python3 tools/concat_kernel.py && ./target/debug/cato build selfhost/cato.cato -o /tmp/cato1h`
Then `/tmp/cato1h build /tmp/bk2.cato /tmp/bk2` → expect E304; `/tmp/cato1h build /tmp/bk3.cato /tmp/bk3` → expect success; `/tmp/cato1h build /tmp/bk4.cato /tmp/bk4` → expect non-boxed rejection.
Run `/tmp/t1.cato`–`/tmp/t6.cato` with the rebuilt compiler.
Expected: bk2 rejected, bk3 accepted, bk4 rejected, t1–t6 PASS.

- [ ] **Step 4: Commit**

```bash
git add selfhost/cato/hir/moveck.cato selfhost/cato/hir/expand.cato selfhost/cato.cato
git commit -m "feat(kernel): mark borrowed call results; E304 tail check"
```

---

### Task 6: Verify the feature is present-but-unused — fixpoint unchanged

**Files:** none (verification task).

- [ ] **Step 1: Rebuild the whole chain**

Run: seed → `/tmp/cato1h`; `/tmp/cato1h build selfhost/cato.cato -o /tmp/cato2 --emit-ll /tmp/cato2.ll`; then `/tmp/cato2 … -o /tmp/cato3`, `/tmp/cato3 … -o /tmp/cato4`.

- [ ] **Step 2: Assert the fixpoint and tests**

Run: `cmp /tmp/cato2.ll /tmp/cato3.ll` (and cato3/cato4). Expected: identical. Run `/tmp/cato2`–`/tmp/cato4` on `/tmp/t1.cato`–`/tmp/t6.cato`. Expected: PASS. Confirm drop counts still `cato1h.ll == 1360`, `cato2.ll == cato3.ll == cato4.ll`.

- [ ] **Step 3: Commit if any generated files tracked**

Only if the repo tracks generated artifacts; otherwise no commit. Use the spec's `Review Focus` item 5 as the check: seed and kernel agree on accept/reject.

---

### Task 7: Apply borrow returns to projecting accessors

**Files:**
- Modify: `selfhost/cato/hir/hexpr.cato` (`hv-a`/`hv-b`/`hv-c`, `hargs-of`, `arms-of`, `name-of`)
- Modify: `selfhost/cato/hir/forms.cato` (`vlist-at` 180, `adts-at` 127, `sigs-at` 150, `varnt-fields-at` 1132)
- Modify: `selfhost/cato/emit/state.cato` (`esc-get` 44, `fs-sum` 440, `fs-pay-vi`)
- Modify: `selfhost/cato/hir/types.cato` (`tys-idx` 133)
- Test: rebuild + t1–t6 + fixpoint

**Interfaces:**
- Consumes: borrowed-return feature (Tasks 1–6).
- Produces: each of the listed accessors becomes `:- &T` (boxed `T`) and drops its internal `(clone …)`; call sites drop the now-unnecessary `(clone …)`.

- [ ] **Step 1: Convert the accessors**

Change signatures to `:- &T`, strip the `(clone …)` in the body's returned field, and remove the `(clone (accessor …))` at call sites where the result is only read. Keep a `(clone …)` where the result must be owned (stored long-term or passed to an owned param). Do a few at a time (`hv-b`, `hv-c`, `tys-idx`, `vlist-at`, `adts-at` first).

- [ ] **Step 2: Regenerate, rebuild, test**

Run: `python3 tools/concat_kernel.py && ./target/debug/cato build selfhost/cato.cato -o /tmp/cato1h`, then rebuild cato2/cato3.
Expected: builds; t1–t6 PASS; `cato2.ll == cato3.ll`.

- [ ] **Step 3: Commit**

```bash
git add selfhost/cato/ selfhost/cato.cato
git commit -m "perf(kernel): projecting accessors return borrows"
```

---

### Task 8: Apply borrow returns to read-only traversals

**Files:**
- Modify: `selfhost/cato/hir/moveck.cato` (`b-count-vars`, `b-count-binds`, `b-tail-escape`, `b-tail-escape-arms`, `contains-recur`, `b-kids`, `b-shadowed-of`)
- Modify: depth/query helpers in `types.cato`, `forms.cato`, `expand.cato` (`tys-depth`, `defns-depth`, `adts-depth`, `sigs-depth`, `vlist-depth`, `plist-depth`, `hes-depth`, `he-count`, …)
- Test: rebuild + t1–t6 + fixpoint

**Interfaces:**
- Consumes: dead-cloned accessors from Task 7.
- Produces: read-only walks take `&HExpr`/`&Exprs` and thread borrowed children; no per-node clone.

- [ ] **Step 1: Convert the traversals**

Change each traversal to take its `HExpr`/`HExprs` argument by `&` and reach children via the borrowed accessors (no `(clone …)`); the recursion passes fields bare. Keep the result accumulators owned.

- [ ] **Step 2: Regenerate, rebuild, test**

Run: `python3 tools/concat_kernel.py && ./target/debug/cato build selfhost/cato.cato -o /tmp/cato1h`, rebuild cato2/cato3.
Expected: builds; t1–t6 PASS; `cato2.ll == cato3.ll`.

- [ ] **Step 3: Commit**

```bash
git add selfhost/cato/ selfhost/cato.cato
git commit -m "perf(kernel): read-only traversals walk borrows (no per-node clone)"
```

---

### Task 9: Measure peak + ASAN

**Files:** none (measurement/verification task).

- [ ] **Step 1: Measure peak RSS**

Rebuild cato2→cato3 while polling `ps -o rss= -p <pid>`. Record the peak.
Expected: materially below the current ~9.02 GB baseline.

- [ ] **Step 2: Re-run the clone-attribution harness**

Re-run the malloc-caller instrumentation (`/tmp/memcount.c` + `/tmp/cato2i`). Expected: the top `clone__HExpr`/`clone__TyList`/`clone__HExprs` counts drop sharply.

- [ ] **Step 3: ASAN the self-host chain**

Run: `clang -O0 -g -fsanitize=address /tmp/kX.ll runtime/runtime.c -o /tmp/kXasan` at the stages; run t1–t6 and a bootstrap stage. Expected: clean (no over-free/UAF).

- [ ] **Step 4: Record results**

Append the measured before/after peak and top-clone deltas to the spec's status header or a `docs/superpowers/` note. Commit.

```bash
git add docs/
git commit -m "docs: borrowed returns — measured peak/clone reduction"
```

---

## Sequencing note

Tasks 1–3 (seed) must land before Task 4 can be validated, and Tasks 4–6 before any kernel source uses `&` (Tasks 7–8). Task 6 is the gate that both compilers agree. Task 9 quantifies the win. The spec's follow-on change **(C)** — de-functionalizing remaining hot structures — is a **separate plan**, written after Task 9 reports what remains hot.
