# Cato: Control Flow, Comparisons & Threading Macros — Design Spec

**Date:** 2026-09-08
**Status:** Draft for review
**Base project spec:** `2026-09-07-cato-language-design.md` (the Cato language design)
**Predecessor plan:** `2026-09-07-cato-minimal-slice.md` (completed; 6/6 tasks shipped)

---

## 1. Goal

Extend the Cato compiler from its completed minimal slice with the missing control
flow that the language spec's §5 examples need:

- a `bool` type and boolean operators (`=`, `/=`, `<`, `>`, `<=`, `>=`, `and`, `or`, `not`),
- conditional expressions (`if`),
- scalar builtins (`abs`, `min`, `max`),
- typed data flow through fused `map`/`filter` stages (f64 pipeline stages),
- the spec §2 `->` / `->>` threading macros (a macro-expansion pipeline stage),
- a second reference program implementing spec §5's `normalize` example, end-to-end.

All new control flow stays **fusable**: an `if`/`and`/`or` inside a fused loop is
emitted as phi-merges inside the single loop, preserving the no-allocation
invariant and the `noalias`/`inbounds`/`align 8` emission rules.

## 2. Scope

**In:**

- `Ty::Bool` (LLVM `i1`).
- Comparison operators over `i64`/`f64` (matches-only on equality; `f64` operands
  promote `i64` operands, exactly like the existing `promote_bin`).
- `if` with exactly one condition and two typed branches; result type is the
  branch type (promoted to `f64` when one branch is `f64`; `b0`/`true`/`false`
  branches may be `bool`).
- `and`, `or` (phi-exact short-circuit, both operands `bool`), `not`.
- Builtins `abs`, `min`, `max` (2-ary), dispatching on operand type
  (`i64` or `f64`); `(abs x)` is a unary node.
- Fused pipeline stage typing: `MapSpec`/`FilterSpec` gain an element type, so an
  f64-typed stage converts the i64 element once (`sitofp`) and runs its body in
  float context; filters branch on a real `i1` predicate (no more i64 0/nonzero).
- `even?` becomes bool-typed.
- Macro stage: `->` / `->>` threading rewrite between reader and HIR build.
- `examples/normalize.cato` reference program + a `count-even-below` control-flow
  program, both compiled and run end-to-end via `clang`.
- Loop captures typed from the enclosing defn's parameter types (replaces the
  "every capture is `f64`" default in `codegen.rs`).

**Out (documented follow-ups, not this slice):**

- General polymorphism / `(Vector T)` monomorphization.
- `deftype` / `match`; map/set literals; `get`/`conj`/`subvec`; `take`/`drop`/`zip`/`range`.
- Ownership & borrow checking (spec §3).
- Macros beyond `->`/`->>` (`when`, `if-let`, `for`, `doseq`, user macros).
- Loops over non-`(Vector i64)` sources; captures of `let`-bound locals into a
  loop inside the same fn (only fn-param captures are supported — see §6).
- `if` in a statement (void) position, `nil`, truthiness of `i64` (conditions
  must be `bool`).

## 3. Pipeline

```
.cato source
  → [Reader] ast::Expr                        (unchanged)
  → [Macro]  expand::expand_all               (NEW) rewrites -> / ->>
  → [HIR]    hir::build                        (new nodes, promotion, stage typing)
  → [Fusion] fuse::check                       (recurse over new nodes)
  → [Emit]   codegen::emit_module              (emit_bool, If/And/Or phis, typed stages)
  → .ll → clang → native binary
```

## 4. Macro expansion stage (`src/expand.rs`)

`pub fn expand_all(prog: &[Expr]) -> Result<Vec<Expr>, String>`.

- Recursively visits every form; a form headed `->` or `->>` is rewritten by
  threading the previous result into each subsequent step:
  - `->`: `(-> x (f a))` → `(f x a)`; bare step `(-> x f)` → `(f x)`;
    zero-arg step `(-> x (f))` → `(f x)`.
  - `->>`: same, but the previous result becomes the **last** argument:
    `(->> xs (map f))` → `(map f xs)`.
- Steps inside the thread body are themselves macro-expanded first.
- Traversal runs bottom-up: children of every list are expanded before the head
  is examined.
- Errors: `->`/`->>` with no steps; a syntax-quote/unknown head is left untouched
  (unknown heads are later rejected by `hir::build` as today).

Threaded example (the normalize chain):

```
(->> xs
     (map (fn [x] (/ (- (f64 x) xsrng) xsrng)))
     (map (fn [x] (clamp-x x -1.0 1.0)))
     (reduce (fn [acc x] (max acc (abs x))) 0.0))
```
expands to
```
(reduce (fn [acc x] (max acc (abs x))) 0.0
  (map (fn [x] (clamp-x x -1.0 1.0))
    (map (fn [x] (/ (- (f64 x) xsrng) xsrng))
      xs)))
```

## 5. Type system & HIR changes (`src/hir.rs`)

### 5.1 `Ty::Bool`

Added to `Ty`. Used by: comparison results, `and`/`or`/`not`, `even?`, `if`
with boolean branches, `let` bindings of boolean values.

### 5.2 New HIR nodes

```rust
pub enum CmpOp { Eq, Ne, Lt, Gt, Le, Ge }

pub enum HExpr {
    // ... existing variants ...
    If(Box<HExpr>, Box<HExpr>, Box<HExpr>, Ty), // cond, then, else, result type
    And(Box<HExpr>, Box<HExpr>),
    Or(Box<HExpr>, Box<HExpr>),
    Not(Box<HExpr>),
    Cmp(CmpOp, Box<HExpr>, Box<HExpr>, bool), // …, f64-promoted flag
    Abs(Box<HExpr>, Ty),                       // operand, result type
    Min(Box<HExpr>, Box<HExpr>, Ty),
    Max(Box<HExpr>, Box<HExpr>, Ty),
}
```

`Cmp` carries an explicit `bool` "compare as f64" flag and `Abs`/`Min`/`Max`
carry their result `Ty`, so codegen never has to re-infer operand types.

**`HReduce::Closure` gains pipeline stages.** Today `lower_reduce` only peels
`map`/`filter` chains when the reduce head is a symbol op (`(reduce + 0 (map …))`);
a closure reduce whose source is a `(map …)`/`(filter …)` chain is rejected.
That is exactly the shape the threading-macro'd `normalize` chain expands to
(`(reduce (fn …) 0.0 (map f2 (map f1 xs)))`), so it must be supported:

```rust
pub enum HReduce {
    Pipeline { combine: BinOp, init: HExpr, maps: Vec<MapSpec>, filters: Vec<FilterSpec>, src: Box<HExpr> },
    Closure {
        init: HExpr, src: Box<HExpr>,
        acc: String, el: String, body: HExpr,
        maps: Vec<MapSpec>, filters: Vec<FilterSpec>, // empty for a plain closure
    },
}
```

A pipeline-backed closure fuses into the single loop: filters run on the raw
element, the map chain runs, then the **closure body** (with `acc`/`el` bound to
the accumulator and the final mapped value) replaces the op `combine`.

`MapSpec` / `FilterSpec` gain exact element and result types:

```rust
pub struct MapSpec    { pub el: String, pub body: HExpr, pub el_ty: Ty, pub res_ty: Ty }
pub struct FilterSpec { pub el: String, pub body: HExpr, pub el_ty: Ty }
```

- `el_ty` is the type of the value flowing **into** that stage: `i64` for the
  first stage over a `(Vector i64)`, otherwise the previous stage's `res_ty`.
- `res_ty` is the type of the stage's output value (the body's result type).
- Filter bodies are always `Ty::Bool`; in this slice all filters run on the raw
  element (`el_ty = i64`) before any map stage — interleaved
  filter-after-map ordering is a documented simplification (unchanged from the
  current fusion pass), so `FilterSpec` needs no `res_ty`.

### 5.3 Lowering rules (`lower` / `lower_reduce` / `lambda_spec`)

- `if`: exactly `(if cond then else)`; `cond` must be bool-typed; result type =
  promote of the two branch types (`f64` if either is `f64`, else the common
  type, which may be `i64` or `bool`). A non-`f64` branch is wrapped in
  `ConvF64` when the result is `f64`.
- Comparison heads `= / /= < > <= >=`: exactly two operands; operands promoted
  like `promote_bin` (wrap the non-`f64` side in `ConvF64` when either is `f64`);
  emit `Cmp`.
- `and` / `or`: exactly two operands; each must be bool-typed (reject `i64`
  truthiness for this slice). `/not`: one bool operand.
- `abs`: one operand; result type = operand type. `min`/`max`: two operands,
  promoted; result type = operand type. `clamp` is **not** a builtin.
- `even?` now lowers to `Cmp(Eq, (BitAnd x 1), 0)`-equivalent bool (implementation
  note: keep it a dedicated `Call("even?")` or emit `Cmp`; either is acceptable,
  but its type is `Ty::Bool`, not `i64`).
- `ty_of` handles all new nodes: `If`→stored type; `And/Or/Not/Cmp`→`Ty::Bool`;
  `Abs/Min/Max`→operand type.
- Upper fns (defns) may have bool params/returns via the existing `:-` syntax
  (`param :- bool`); `parse_ty` accepts `bool`.
- `lambda_spec` computes `el_ty` for each stage from the stage's input value type.

## 6. Codegen changes (`src/codegen.rs`)

### 6.1 Capture typing

Current `make_loop_spec` types **every** loop capture `f64`
(`loop_free_vars` → all `(v, Ty::F64)`). This correctly typed the slice's only
capture (`mu`) but breaks an `i64` capture (e.g. `n` in `count-even-below`).

New rule: when `rewrite_body` hoists a reduce out of defn `d`, it receives
`&d.params`; each free-var capture is typed by matching its name against
`d.params`; a free var not found in the params (a `let`-bound capture) keeps the
legacy `f64` default and is documented as unsupported for this slice (no such
case exists in the reference programs).

### 6.2 `emit_bool`

New expression context producing `i1`:

- `Cmp`: `i64` operands → `icmp {eq,ne,slt,sgt,sle,sge}`; `f64` operands →
  `fcmp {oeq,one,olt,ogt,ole,oge}`.
- `And`: phi-exact short-circuit — evaluate left, branch to `rhs`/`false`, then
  `phi i1 [false, false-branch], [right, rhs]`.
- `Or`: same shape with `true` in the short-circuit arm.
- `Not`: `xor i1` on true (with `true` produced by comparing against 0 where
  needed).
- `Var` of `Ty::Bool` (incl. `let`-bound bools): read the `i1` register.
- `if` with `bool` result: `emit_bool` on branches, phi merge.
- `even?`: `%p = and i64 %x, 1; %e = icmp eq i64 %p, 0` → `i1`.

Blocks are named from the codegen counter (unique suffixes) — the existing
`apply`/`skip`/`merge` pattern is precedent.

### 6.3 `if` in `emit_int` / `emit_float`

- Cond via `emit_bool`; branch to `then`/`else`; each branch emits its arm in the
  result context; phi merge of the two arm registers; result register
  participates in the enclosing expression (e.g. a loop-carried `acc` phi can
  read the `if`'s phi directly).

### 6.4 `Abs` / `Min` / `Max`

- `Abs` i64: `%neg = sub i64 0, %v`; `select (icmp slt %v, 0) %neg %v`.
- `Abs` f64: `%neg = fneg double %v`; `select (fcmp olt %v, 0.0) %neg %v`.
- `Min`/`Max`: `icmp slt/sgt` (i64) or `fcmp olt/ogt` (f64) + `select`.
  No `llvm.abs`/intrinsics.

### 6.5 Typed pipeline stages

Today `emit_loop` runs every map body through `emit_int` and every filter
predicate through `emit_int` + `icmp ne i64 {{pred}}, 0`.

New:

- The element register `%x` is loaded as `i64` as today. The *incoming value*
  `val` for stage 1 is `%x` (ty `i64`); each stage's incoming type is its
  `el_ty`.
- A map stage whose `res_ty` is `f64` first converts the incoming value once
  (`sitofp i64` only when the incoming `el_ty` is `i64`) and binds `el` to that
  register as `Ty::F64`; the body is then emitted with `emit_float`. An
  `el_ty == f64` stage (fed by a previous f64 stage) binds `el` straight to the
  incoming register. An `res_ty == i64` stage emits with `emit_int`.
- Filters: predicate emitted with `emit_bool`, `br i1 <pred>` directly (drops
  `icmp ne`). Multiple filters branch sequentially as today.
- The combine: existing `combine_step` already picks the op by accumulator type;
  the last map stage's output must match the accumulator type (guaranteed by HIR
  stage typing).
- Closure reduces (`HReduce::Closure`) already choose `emit_float`/`emit_int` by
  the init type. A pipeline-backed closure (maps/filters non-empty) emits the
  chain then runs the closure body as the combine — `acc`/`el` bound as today —
  instead of `combine_step`. A boolean `if` inside an `i64` closure body emits
  via the new `emit_int` `If` path (e.g. `count-even-below`).

### 6.6 Pattern updates required

`rewrite_body`, `collect_vars`, and `fuse::check` must recurse into every new
node (`If`, `And`, `Or`, `Not`, `Cmp`, `Abs`, `Min`, `Max`). Vec-literal globals,
`noalias`/`inbounds`/`align 8` emission, and `main` i32 are unchanged.

## 7. Reference programs

### 7.1 `examples/normalize.cato`

Implements spec §5's `normalize` example (signature bug in the spec — it threads
`xs` but declares `[xsmean xsrng]` and uses an undefined `clamp` — is corrected:
the bound is `xs`, and `clamp` is written in Cato as `clamp-x`).

```clojure
; normalize.cato — spec §5 normalize example. map/map/reduce fuses to ONE loop.

(defn clamp-x [x :- f64 lo :- f64 hi :- f64] :- f64
  (min (max x lo) hi))

(defn normalize [xs :- vec xsrng :- f64] :- f64
  (->> xs
       (map (fn [x] (/ (- (f64 x) xsrng) xsrng)))
       (map (fn [x] (clamp-x x -1.0 1.0)))
       (reduce (fn [acc x] (max acc (abs x))) 0.0)))

(defn main []
  (print-f64 (normalize (vec-i64 2 4 6) 8.0)))
```

**Expected output:** `0.75`

Derivation (all exact dyadic values): stage 1 → (2−8)/8 = −0.75, −0.5, −0.25;
stage 2 unchanged; reduce → `max(acc, |x|)` = max(0.75, 0.5, 0.25) = 0.75.
Emitted via `printf("%g")` → `0.75`.

### 7.2 `count-even-below` (embedded in tests, not `examples/`)

```clojure
(defn count-even-below [xs :- vec n :- i64] :- i64
  (reduce (fn [acc x] (if (and (even? x) (< x n)) (+ acc 1) acc)) 0 xs))

(defn main []
  (print-i64 (count-even-below (vec-i64 1 2 3 4 5 6) 6)))
```

Exercises `if` + `and` + `<` + an `i64` loop capture, all inside a fused closure
reduce. **Expected output:** `2` (evens below 6: 2 and 4).

## 8. Testing

### 8.1 New `tests/expand_test.rs`

- `->` with paren step: `(-> x (f a))` → `(f x a)`.
- `->` with bare/zero-arg steps.
- `->>` last-arg threading: normalize chain shape.
- Macro-expansion inside nested forms and inside `defn` bodies.
- Errors: empty thread body.

### 8.2 `tests/hir_test.rs` additions

- `if` lowering + branch promotion to `f64`.
- Comparison lowering; f64 promotion of mixed operands.
- `and`/`or`/`not` lowering; rejection of non-bool operands.
- `abs`/`min`/`max` result typing.
- `parse_ty("bool")`.
- `MapSpec.el_ty`/`res_ty` set correctly through an f64 map chain (stage 1
  `el_ty=i64`/`res_ty=f64`, stage 2 `el_ty=f64`).
- A closure reduce over a `(map …)`/`(filter …)` source lowers to a
  pipeline-backed `HReduce::Closure` (maps/filters non-empty).

### 8.3 `tests/codegen_test.rs` additions & updates

- `emit_bool` for a comparison; `icmp slt`/`fcmp olt` presence.
- `if` emits a phi merge; cond branched on `i1`.
- `and`/`or` short-circuit blocks.
- f64 map stage emits `sitofp` once and `fsub/fdiv double`; filter branches on
  `i1` directly.
- `even?` emits `i1` (`icmp eq`) — **update** the two existing even-? assertions
  (`and i64` / `xor i64` / `icmp ne i64` will change shape).
- Captured `i64` loop param typed `i64` (not `f64`).

### 8.4 `tests/integration.rs` additions

- `compiles_normalize_to_expected_output`: build `examples/normalize.cato` →
  IR → `clang` → run → assert `0.75\n` (mirrors the existing slice e2e).
- `compiles_count_even_below_to_expected_output`: compile the 7.2 program
  (written as a string to `target/`) → `clang` → run → assert `2\n`.

### 8.5 Existing tests

All existing slice tests must stay green, with the two even-? codegen assertions
updated (§8.3). `main.rs` test helpers and any `build(&read_all(...))` call
sites gain the `expand_all` step.

## 9. File structure

```
src/expand.rs                          (new) macro-expansion pass
examples/normalize.cato                (new) reference program
tests/expand_test.rs                   (new) macro tests
src/hir.rs                             (modify) Ty::Bool, nodes, lowering, stage typing
src/codegen.rs                         (modify) emit_bool, If/And/Or, typed stages, capture typing
src/fuse.rs                            (modify) recurse over new nodes
src/main.rs                            (modify) insert expand_all
src/lib.rs                             (modify) pub mod expand;
tests/hir_test.rs tests/codegen_test.rs tests/integration.rs   (modify)
```

## 10. Constraints

- Rust, edition 2021, std only (no new crates).
- Emission stays target-agnostic; system `clang` sets the target.
- All loop vector-data args remain `noalias nocapture readonly`; GEPs `inbounds`;
  loads `align 8`.
- Every reference program is a fused single loop where applicable (the
  `normalize` program's map/map/reduce must remain one loop).
- Every task ends with `cargo build` green, its tests passing, then a commit.
- Run tests with `cargo test`; link programs with `clang`.

## 11. Deliverables

1. Macro-expansion stage with tests.
2. `bool` type, comparisons, `if`, `and`/`or`/`not`, `abs`/`min`/`max` —
   lowered in HIR, emitted with phi-exact control flow.
3. Typed pipeline stages (f64 map stages, `i1` filter predicates).
4. Param-based loop capture typing.
5. `examples/normalize.cato` compiling and printing `0.75`; `count-even-below`
   printing `2`, both through `clang` end-to-end.