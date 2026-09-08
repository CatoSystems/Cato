# Cato Compiler — Control Flow, Comparisons & Threading Macros Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the working Cato compiler (minimal slice, shipped) with `bool` conditions and control flow — comparisons (`= /= < > <= >=`), `if`, `and`/`or`/`not`, `abs`/`min`/`max` — plus the spec §2 `->`/`->>` threading macros and typed data flow through fused `map`/`filter` stages, all fusing into the existing single allocation-free loop with `noalias` emission.

**Architecture:** Same native binary pipeline, with ONE new stage inserted between reader and HIR build:

```
.cato source → Reader → [expand::expand_all] → hir::build → fuse::check → codegen::emit_module → .ll → clang → binary
```

Control flow is implemented as phi-exact HIR nodes (`If`/`And`/`Or`/`Not`/`Cmp`) so fused loops keep a single allocation-free body. A closure `reduce` whose source is a `map`/`filter` chain (the `->>`-expanded `normalize` shape) becomes a pipeline-backed closure — one loop, filters then maps then the closure body as combine.

**Tech Stack:** Rust (edition 2021), std only, system `clang`.

**Spec:** `docs/superpowers/specs/2026-09-08-cato-control-flow-and-macros-design.md`

## Global Constraints

- Language: Rust, edition 2021, no external crates.
- Emit platform-agnostic LLVM IR; system `clang` sets the target. No hardcoded triple.
- Every loop vector-data pointer stays `noalias nocapture readonly`; GEPs `inbounds`; loads `align 8`.
- Numbers are `i64`/`f64`; new `bool` is LLVM `i1`. Conditions must be `bool` (no `i64` truthiness in this slice).
- `clamp` is NOT a builtin — written in Cato as `(min (max x lo) hi)`.
- Expected reference outputs: `examples/normalize.cato` → exactly `0.75\n`; the `count-even-below` program → exactly `2\n`. The existing `examples/slice.cato` output `2.5\n20\n` must not change.
- Every task ends with `cargo build` green, its tests passing, then a commit.
- Run tests with `cargo test`. Link programs with `clang examples/x.ll runtime/runtime.c -o /tmp/cato_x`.

---

## File Structure

```
examples/
  normalize.cato             # (new) reference program → prints 0.75
src/
  expand.rs                  # (new) AST→AST macro pass: -> / ->>
  hir.rs                     # (mod) Ty::Bool, CmpOp, If/And/Or/Not/Cmp/Abs/Min/Max,
                             #   closure-pipeline reduces, MapSpec/FilterSpec el_ty/res_ty
  fuse.rs                    # (mod) recurse over new nodes
  codegen.rs                 # (mod) emit_bool, If/And/Or phis, typed stages, capture typing, even?→i1
  main.rs                    # (mod) insert expand_all
  lib.rs                     # (mod) pub mod expand;
tests/
  expand_test.rs             # (new) macro expansion tests
  hir_test.rs                # (mod) new-node lowering tests
  codegen_test.rs            # (mod) emit_bool/if/stage tests + even? assertion updates
  integration.rs             # (mod) normalize + count-even-below e2e
```

---

### Task 1: Macro Expansion Stage (`->` / `->>`)

**Files:**
- Create: `src/expand.rs`
- Modify: `src/lib.rs`, `src/main.rs`
- Create: `tests/expand_test.rs`

**Interfaces:**
- Produces: `pub fn expand_all(prog: &[Expr]) -> Result<Vec<Expr>, String>` in `expand.rs`, rewrites `->`/`->>` anywhere in the tree (children expanded first, bottom-up). `->` inserts the previous result as the **first** argument of each step; `->>` as the **last**.

- [ ] **Step 1: Write the failing test** — `tests/expand_test.rs`:

```rust
use cato::ast::Expr;
use cato::expand::expand_all;
use cato::reader::read_all;

fn expand_one(src: &str) -> Expr {
    let mut v = expand_all(&read_all(src).unwrap()).unwrap();
    assert_eq!(v.len(), 1);
    v.pop().unwrap()
}

fn list(items: Vec<Expr>) -> Expr { Expr::List(items) }
fn sym(s: &str) -> Expr { Expr::LitSym(s.to_string()) }

#[test]
fn threads_first_position() {
    assert_eq!(expand_one("(-> x (f a))"), list(vec![sym("f"), sym("x"), sym("a")]));
}

#[test]
fn threads_bare_symbol_step() {
    assert_eq!(expand_one("(-> x f)"), list(vec![sym("f"), sym("x")]));
}

#[test]
fn threads_zero_arg_step() {
    assert_eq!(expand_one("(-> x (f))"), list(vec![sym("f"), sym("x")]));
}

#[test]
fn threads_last_position() {
    assert_eq!(expand_one("(->> xs (map f))"), list(vec![sym("map"), sym("f"), sym("xs")]));
}

#[test]
fn threads_last_position_with_args() {
    assert_eq!(expand_one("(->> xs (f a))"), list(vec![sym("f"), sym("a"), sym("xs")]));
}

#[test]
fn expands_normalize_chain() {
    // (->> xs (map g) (map h) (reduce r 0.0))
    //   -> (reduce r 0.0 (map h (map g xs)))
    assert_eq!(
        expand_one("(->> xs (map g) (map h) (reduce r 0.0))"),
        list(vec![
            sym("reduce"), sym("r"), Expr::LitF64(0.0),
            list(vec![sym("map"), sym("h"),
                list(vec![sym("map"), sym("g"), sym("xs")])]),
        ])
    );
}

#[test]
fn expands_inside_defn_body() {
    let v = expand_all(&read_all("(defn f [xs :- vec] (->> xs (map g)))").unwrap()).unwrap();
    assert_eq!(v.len(), 1);
}

#[test]
fn rejects_empty_thread() {
    assert!(expand_all(&read_all("(->)").unwrap()).is_err());
    assert!(expand_all(&read_all("(->>)").unwrap()).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test expand_test`
Expected: FAIL — module `cato::expand` not found.

- [ ] **Step 3: Implementation**

`src/expand.rs`:

```rust
use crate::ast::Expr;

/// Rewrite every `->` / `->>` form into nested application, bottom-up.
/// `->`   threads the previous result as the first argument of each step.
/// `->>`  threads it as the last argument.
pub fn expand_all(prog: &[Expr]) -> Result<Vec<Expr>, String> {
    prog.iter().map(expand_form).collect()
}

fn expand_form(e: &Expr) -> Result<Expr, String> {
    let Expr::List(items) = e else { return Ok(e.clone()) };
    let items: Vec<Expr> = items.iter().map(expand_form).collect::<Result<_, _>>()?;
    let thread_pos = match items.first() {
        Some(Expr::LitSym(h)) if h == "->" => Some(false),
        Some(Expr::LitSym(h)) if h == "->>" => Some(true),
        _ => None,
    };
    match thread_pos {
        None => Ok(Expr::List(items)),
        Some(last) => expand_thread(&items[1..], last),
    }
}

fn expand_thread(steps: &[Expr], last: bool) -> Result<Expr, String> {
    if steps.is_empty() {
        return Err("threading macro expects at least one step".into());
    }
    let mut acc = steps[0].clone();
    for step in &steps[1..] {
        acc = apply_step(acc, step, last)?;
    }
    Ok(acc)
}

fn apply_step(prev: Expr, step: &Expr, last: bool) -> Result<Expr, String> {
    match step {
        Expr::List(items) => {
            let mut out = items.clone();
            if last { out.push(prev); } else { out.insert(1, prev); }
            Ok(Expr::List(out))
        }
        Expr::LitSym(_) => Ok(Expr::List(vec![step.clone(), prev])),
        _ => Err("threading step must be a list or symbol".into()),
    }
}
```

`src/lib.rs`: add `pub mod expand;`.

`src/main.rs`: insert the stage between read and build:

```rust
    let parsed = cato::reader::read_all(&src)
        .unwrap_or_else(|e| { eprintln!("cato: parse error: {}", e); std::process::exit(2); });
    let expanded = cato::expand::expand_all(&parsed)
        .unwrap_or_else(|e| { eprintln!("cato: macro error: {}", e); std::process::exit(2); });
    let fns = cato::hir::build(&expanded)
        .unwrap_or_else(|e| { eprintln!("cato: semantic error: {}", e); std::process::exit(2); });
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS (expand_test + all existing tests; `slice.cato` contains no threading forms so it expands to itself).

- [ ] **Step 5: Commit**

```bash
git add src/expand.rs src/lib.rs src/main.rs tests/expand_test.rs
git commit -m "feat: -> and ->> macro expansion stage"
```

---

### Task 2: HIR — Bool Type, Control-Flow Nodes, Pipeline-Backed Closures, Stage Typing

**Files:**
- Modify: `src/hir.rs`
- Modify: `src/fuse.rs` (recurse over new nodes — required for exhaustive match)
- Modify: `src/codegen.rs` (recursion in `rewrite_body`/`collect_vars`; **stub** emit arms so the crate compiles; real emission lands in Task 3)
- Modify: `tests/hir_test.rs`

**Interfaces:**
- `Ty::Bool` added.
- New `HExpr` variants (each carries its result type so codegen never re-infers):
  `If(cond, then, els, Ty)`, `And(Box, Box)`, `Or(Box, Box)`, `Not(Box)`,
  `Cmp(CmpOp, Box, Box, bool /*f64 flag*/)`, `Abs(Box, Ty)`, `Min(Box, Box, Ty)`, `Max(Box, Box, Ty)`.
- `HReduce::Closure` gains `maps: Vec<MapSpec>` / `filters: Vec<FilterSpec>` (empty for plain closures) — a closure reduce whose source is a `map`/`filter` chain is lowered as a pipeline-backed closure.
- `MapSpec { el, body, el_ty, res_ty }`, `FilterSpec { el, body, el_ty }`.
- `parse_ty` accepts `"bool"`.
- `even?`'s result type is `Ty::Bool`.

- [ ] **Step 1: Write the failing test** — append to `tests/hir_test.rs`:

```rust
#[test]
fn lowers_if_with_promotion() {
    // then branch is i64, else is f64 -> result f64, i64 wrapped in ConvF64
    let body = build_one("f", "(defn f [x :- i64] :- f64 (if (< x 2) x 1.0))");
    let HExpr::If(cond, then, els, ty) = body else { panic!("expected if") };
    assert_eq!(ty, Ty::F64);
    assert!(matches!(*cond, HExpr::Cmp(CmpOp::Lt, _, _, _)));
    assert!(matches!(*then, HExpr::ConvF64(_)));
    assert!(matches!(*els, HExpr::ConstF64(_)));
}

#[test]
fn lowers_comparison_int() {
    let body = build_one("gt", "(defn gt [a :- i64 b :- i64] :- bool (> a b))");
    assert!(matches!(body, HExpr::Cmp(CmpOp::Gt, _, _, false)));
}

#[test]
fn lowers_mixed_comparison_promotes_f64() {
    // i64 < f64: the i64 side wraps in ConvF64, the f64 constant stays put
    let body = build_one("f", "(defn f [a :- i64] :- bool (< a 2.5))");
    let HExpr::Cmp(_, a, b, f64flag) = body else { panic!("expected cmp") };
    assert!(f64flag);
    assert!(matches!(*a, HExpr::ConvF64(_)));
    assert!(matches!(*b, HExpr::ConstF64(_)));
}

#[test]
fn lowers_and_or_not() {
    let body = build_one("f",
        "(defn f [b :- bool c :- bool] :- bool (and b (or c (not b))))");
    let HExpr::And(a1, o1) = body else { panic!("expected and") };
    assert!(matches!(*a1, HExpr::Var(_))); // "b"
    let HExpr::Or(a2, n1) = *o1 else { panic!("expected or") };
    assert!(matches!(*a2, HExpr::Var(_))); // "c"
    let HExpr::Not(v) = *n1 else { panic!("expected not") };
    assert!(matches!(*v, HExpr::Var(_)));  // "b"
}

#[test]
fn rejects_non_bool_and_operand() {
    assert!(build(&read_all("(defn f [x :- i64] :- bool (and (< x 3) x))").unwrap()).is_err());
}

#[test]
fn lowers_abs_min_max() {
    let body = build_one("f",
        "(defn f [a :- i64 b :- i64] :- i64 (min (abs a) b))");
    let HExpr::Min(inner, r, ty) = body else { panic!("expected min") };
    assert_eq!(ty, Ty::I64);
    assert!(matches!(*r, HExpr::Var(_)));
    let HExpr::Abs(a, at) = *inner else { panic!("expected abs") };
    assert_eq!(at, Ty::I64);
    assert!(matches!(*a, HExpr::Var(_)));
}

#[test]
fn closure_reduce_over_map_chain_is_pipeline_backed() {
    let body = build_one("n",
        "(defn n [xs :- vec] :- f64
           (reduce (fn [acc x] (+ acc x)) 0.0
             (map (fn [x] (f64 x)) xs)))");
    let HExpr::Reduce(r) = body else { panic!("expected reduce") };
    match *r {
        HReduce::Closure { maps, filters, src, .. } => {
            assert_eq!(maps.len(), 1);
            assert_eq!(filters.len(), 0);
            assert!(matches!(*src, HExpr::Var(_)));
        }
        _ => panic!("expected closure reduce"),
    }
}

#[test]
fn map_stage_types_flow_through_chain() {
    // clamp-x must be DEFINED so `build` can resolve its f64 return type;
    // it is referenced (not used as a map fn, so its 3 params are fine).
    let body = build_one("n",
        "(defn clamp-x [x :- f64 lo :- f64 hi :- f64] :- f64 (min (max x lo) hi))
         (defn n [xs :- vec xsrng :- f64] :- f64
           (reduce (fn [acc x] (max acc (abs x))) 0.0
             (map (fn [x] (clamp-x x -1.0 1.0))
               (map (fn [x] (/ (- (f64 x) xsrng) xsrng)) xs))))");
    let HExpr::Reduce(r) = body else { panic!("expected reduce") };
    let HReduce::Closure { maps, .. } = *r else { panic!("expected closure") };
    // inner-most stage is i64 -> f64
    assert_eq!(maps[0].el_ty, Ty::I64);
    assert_eq!(maps[0].res_ty, Ty::F64);
    // outer stage is fed f64
    assert_eq!(maps[1].el_ty, Ty::F64);
    assert_eq!(maps[1].res_ty, Ty::F64);
}

#[test]
fn parses_bool_params_and_not() {
    let body = build_one("f", "(defn f [b :- bool] :- bool (not b))");
    assert!(matches!(body, HExpr::Not(_)));
}
```

Note: `build_one` in the existing `hir_test.rs` already returns the body of a named defn and `build` resolves every defn in the source, so a helper like `clamp-x` used **as a called function** inside a map lambda must be defined in the same source string (its `:- f64` return type resolves through `raw_by_name`). A 3-param fn cannot be a `map`/`filter` fn reference (those need exactly 1 param), but an inline `(fn [x] (clamp-x x -1.0 1.0))` is fine.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test hir_test`
Expected: FAIL — new node/variant compile errors and missing lowering.

- [ ] **Step 3: Implementation**

**`src/hir.rs` — type & data structures:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty { I64, F64, VecI64, Bool }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp { Eq, Ne, Lt, Gt, Le, Ge }
```

```rust
pub enum HExpr {
    // ... existing variants unchanged ...
    If(Box<HExpr>, Box<HExpr>, Box<HExpr>, Ty),
    And(Box<HExpr>, Box<HExpr>),
    Or(Box<HExpr>, Box<HExpr>),
    Not(Box<HExpr>),
    Cmp(CmpOp, Box<HExpr>, Box<HExpr>, bool),
    Abs(Box<HExpr>, Ty),
    Min(Box<HExpr>, Box<HExpr>, Ty),
    Max(Box<HExpr>, Box<HExpr>, Ty),
}

pub struct MapSpec    { pub el: String, pub body: HExpr, pub el_ty: Ty, pub res_ty: Ty }
pub struct FilterSpec { pub el: String, pub body: HExpr, pub el_ty: Ty }

pub enum HReduce {
    Pipeline { combine: BinOp, init: HExpr, maps: Vec<MapSpec>, filters: Vec<FilterSpec>, src: Box<HExpr> },
    Closure {
        init: HExpr, src: Box<HExpr>, acc: String, el: String, body: HExpr,
        maps: Vec<MapSpec>, filters: Vec<FilterSpec>,
    },
}
```

**`parse_ty`:** add `"bool" => Ok(Ty::Bool)`.

**Promotion helpers** (refactor `promote_bin` to share with comparisons/min/max):

```rust
fn coerce_f64(e: HExpr, cur: Ty, want: bool) -> HExpr {
    if want && cur != Ty::F64 { HExpr::ConvF64(Box::new(e)) } else { e }
}

fn promote_pair(a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> (HExpr, HExpr) {
    let at = ty_of(&a, env, ctx);
    let bt = ty_of(&b, env, ctx);
    let f = at == Ty::F64 || bt == Ty::F64;
    (coerce_f64(a, at, f), coerce_f64(b, bt, f))
}

fn promote_bin(op: BinOp, a: HExpr, b: HExpr, env: &Env, ctx: &Ctx) -> HExpr {
    let (a, b) = promote_pair(a, b, env, ctx);
    HExpr::Bin(op, Box::new(a), Box::new(b))
}
```

**New heads in `lower`'s form match:**

```rust
"if" => lower_if(items, env, ctx),
"=" | "/=" | "<" | ">" | "<=" | ">=" => lower_cmp(head, items, env, ctx),
"and" | "or" => lower_and_or(head, items, env, ctx),
"not" => lower_not(items, env, ctx),
"abs" => lower_abs(items, env, ctx),
"min" | "max" => lower_min_max(head, items, env, ctx),
```

**Lowering functions:**

```rust
fn lower_if(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 4 { return Err("if expects (if cond then else)".into()); }
    let cond = lower(&items[1], env, ctx)?;
    if ty_of(&cond, env, ctx) != Ty::Bool {
        return Err("if condition must be boolean".into());
    }
    let then = lower(&items[2], env, ctx)?;
    let els  = lower(&items[3], env, ctx)?;
    let tt = ty_of(&then, env, ctx);
    let et = ty_of(&els,  env, ctx);
    if tt != et && tt != Ty::F64 && et != Ty::F64 {
        return Err(format!("if branches must be the same type, got {:?} and {:?}", tt, et));
    }
    let t = if tt == Ty::F64 || et == Ty::F64 { Ty::F64 } else { tt };
    Ok(HExpr::If(Box::new(cond), Box::new(coerce_f64(then, tt, t == Ty::F64)),
                 Box::new(coerce_f64(els, et, t == Ty::F64)), t))
}

fn lower_cmp(op: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 { return Err(format!("comparison {} expects 2 operands", op)); }
    let op = match op { "=" => CmpOp::Eq, "/=" => CmpOp::Ne, "<" => CmpOp::Lt,
                        ">" => CmpOp::Gt, "<=" => CmpOp::Le, ">=" => CmpOp::Ge,
                        _ => unreachable!() };
    let (a, b) = promote_pair(lower(&items[1], env, ctx)?, lower(&items[2], env, ctx)?, env, ctx);
    let is_f64 = ty_of(&a, env, ctx) == Ty::F64;
    Ok(HExpr::Cmp(op, Box::new(a), Box::new(b), is_f64))
}

fn lower_and_or(kind: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 { return Err(format!("{} expects 2 operands", kind)); }
    let a = lower(&items[1], env, ctx)?;
    let b = lower(&items[2], env, ctx)?;
    if ty_of(&a, env, ctx) != Ty::Bool || ty_of(&b, env, ctx) != Ty::Bool {
        return Err(format!("{} expects boolean operands", kind));
    }
    Ok(if kind == "and" { HExpr::And(Box::new(a), Box::new(b)) }
       else            { HExpr::Or(Box::new(a), Box::new(b)) })
}

fn lower_not(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 2 { return Err("not expects 1 operand".into()); }
    let e = lower(&items[1], env, ctx)?;
    if ty_of(&e, env, ctx) != Ty::Bool { return Err("not expects a boolean operand".into()); }
    Ok(HExpr::Not(Box::new(e)))
}

fn lower_abs(items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 2 { return Err("abs expects 1 operand".into()); }
    let e = lower(&items[1], env, ctx)?;
    let t = ty_of(&e, env, ctx);
    if t != Ty::I64 && t != Ty::F64 { return Err("abs expects an i64 or f64 operand".into()); }
    Ok(HExpr::Abs(Box::new(e), t))
}

fn lower_min_max(kind: &str, items: &[Expr], env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    if items.len() != 3 { return Err(format!("{} expects 2 operands", kind)); }
    let (a, b) = promote_pair(lower(&items[1], env, ctx)?, lower(&items[2], env, ctx)?, env, ctx);
    let t = ty_of(&a, env, ctx);
    if t != Ty::I64 && t != Ty::F64 { return Err(format!("{} expects i64/f64 operands", kind)); }
    Ok(if kind == "min" { HExpr::Min(Box::new(a), Box::new(b), t) }
       else             { HExpr::Max(Box::new(a), Box::new(b), t) })
}
```

**`lower_reduce` closure branch** — peel the source chain (a closure over a `map`/`filter` source becomes pipeline-backed) and seed the closure body env with the *real* element type at its position in the chain:

```rust
_ => {
    let Expr::List(fnitems) = head else {
        return Err("reduce expects a combine op or (fn ...)".into());
    };
    if fnitems.len() < 3 {
        return Err("reduce fn expects (fn [acc el] <body>)".into());
    }
    let Expr::Vector(params) = &fnitems[1] else {
        return Err("reduce fn params".into());
    };
    if params.len() < 2 {
        return Err("reduce fn must bind exactly [acc el]".into());
    }
    let acc = param_name(&params[0])?;
    let el = param_name(&params[1])?;
    let init = lower(&items[2], env, ctx)?;
    let init_ty = ty_of(&init, env, ctx);
    let (src, maps, filters) = pipeline_src(&items[3], env, ctx)?;
    let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
    let mut child = env.clone();
    child.insert(acc.clone(), init_ty);
    child.insert(el.clone(), el_ty);
    let body = lower(&fnitems[2], &child, ctx)?;
    Ok(HExpr::Reduce(Box::new(HReduce::Closure {
        init,
        src: Box::new(src),
        acc,
        el,
        body,
        maps,
        filters,
    })))
}
```

`el_ty` is the type the closure's `el` receives: the `res_ty` of the last (outer-most) map stage, or `I64` for a plain vector source. This keeps `(+ acc x)` / `(max acc (abs x))` correctly typed when a map chain produces f64 values.

**`pipeline_src`** — compute per-stage types (`maps` is inner-first). `lambda_spec` now takes the element type the stage receives (so bodies that do arithmetic on `el` type-check) and returns the stage's result type:

```rust
if head == "map" {
    if items.len() < 3 {
        return Err("map expects (map <fn> <src>)".into());
    }
    let (inner, mut maps, filters) = pipeline_src(&items[2], env, ctx)?;
    let el_ty = maps.last().map(|m| m.res_ty).unwrap_or(Ty::I64);
    let (el, body, res_ty) = lambda_spec(&items[1], el_ty, env, ctx)?;
    maps.push(MapSpec { el, body, el_ty, res_ty });
    Ok((inner, maps, filters))
} else if head == "filter" {
    if items.len() < 3 {
        return Err("filter expects (filter <fn> <src>)".into());
    }
    let (inner, maps, mut filters) = pipeline_src(&items[2], env, ctx)?;
    let (el, body, _) = lambda_spec(&items[1], Ty::I64, env, ctx)?;
    filters.push(FilterSpec { el, body, el_ty: Ty::I64 });
    Ok((inner, maps, filters))
}
```

`lambda_spec(f, el_ty, env, ctx) -> Result<(String, HExpr, Ty), String>` — inline `(fn [el] body)` lowers with `el → el_ty` in the child env and returns `res_ty = ty_of(&body, &child, ctx)`; an fn reference (e.g. `square`) resolves its body the same way and returns the resolved defn's `ret` as `res_ty`. `even?` returns `(el "x", Call("even?", [Var "x"]), Ty::Bool)`. The existing fn bodies keep their current shape — nothing about the fusion invariant changes.

**`ty_of` additions:**

```rust
HExpr::If(_, _, _, t) => *t,
HExpr::And(..) | HExpr::Or(..) | HExpr::Not(_) | HExpr::Cmp(..) => Ty::Bool,
HExpr::Abs(_, t) | HExpr::Min(_, _, t) | HExpr::Max(_, _, t) => *t,
HExpr::Call(name, _) if name == "even?" => Ty::Bool,   // before the ctx lookup arm
```

**`src/fuse.rs`** — add recursion arms to `check` (identical shape to existing):
`If` (all 3, keep `ty`), `And`/`Or` (both), `Not` (inner), `Cmp` (both, keep flag), `Abs` (inner, keep `Ty`), `Min`/`Max` (both, keep `Ty`).

**`src/codegen.rs` — this task only** (must compile; Task 3 fills emission):
- `rewrite_body`: recurse into every new node identically (keep `Ty`/flag).
- `collect_vars`: recurse into every new node.
- `emit_int` / `emit_float`: add a combined arm that panics with a clear message:
  `HExpr::If(..) | HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) | HExpr::Cmp(..) | HExpr::Abs(..) | HExpr::Min(..) | HExpr::Max(..) => panic!("codegen: control-flow emission lands in the next task")`.
  Do NOT change any existing emission yet — all existing tests must pass.
- `Ty::Bool` makes two existing `match`es non-exhaustive; add compile-only stub arms that panic with the same message: `emit_plain`'s param-type `match` (`Ty::Bool => panic!(...)`) and `call_user`'s arg-type `match`. (`emit_for_ret` compiles as-is: Bool falls into its `else` → `emit_int`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS (new hir_test cases + existing slice/codegen/fuse/integration green).

- [ ] **Step 5: Commit**

```bash
git add src/hir.rs src/fuse.rs src/codegen.rs tests/hir_test.rs
git commit -m "feat: hir bool type, control-flow nodes, pipeline-backed closures, stage typing"
```

---

### Task 3: Codegen — Bool Context, If/And/Or Phis, Typed Stages, Capture Typing

**Files:**
- Modify: `src/codegen.rs`
- Modify: `tests/codegen_test.rs` (new tests + update the two existing `even?` assertions)

**Interfaces:**
- `FnCtx` gains block-name tracking (`cur`) so `and`/`or` phi predecessors resolve.
- New `emit_bool(e, c, mdl, sigs) -> String` producing `i1`.
- Unified `emit_typed(e, ty, ...)` routing by `Ty`.
- `emit_loop` emits typed map stages (f64 `sitofp` once), filters on raw `i1`, closure-backed pipelines, and loop captures typed from `params`.

- [ ] **Step 1: Write the failing test** — append to `tests/codegen_test.rs`:

```rust
#[test]
fn if_emits_phi_merge() {
    let ir = emit("(defn f [x :- i64 n :- i64] :- i64 (if (< x n) x n))");
    assert!(ir.contains("icmp slt i64"));
    assert!(ir.contains("phi i64"));
}

#[test]
fn and_emits_short_circuit_phi() {
    let ir = emit(
        "(defn count-even-below [xs :- vec n :- i64] :- i64
           (reduce (fn [acc x] (if (and (even? x) (< x n)) (+ acc 1) acc)) 0 xs))");
    assert!(ir.contains("phi i1"));
    assert!(ir.contains("icmp slt i64"));
}

#[test]
fn f64_map_stage_converts_element_once() {
    let ir = emit(
        "(defn clamp-x [x :- f64 lo :- f64 hi :- f64] :- f64 (min (max x lo) hi))
         (defn normalize [xs :- vec xsrng :- f64] :- f64
           (reduce (fn [acc x] (max acc (abs x))) 0.0
             (map (fn [x] (clamp-x x -1.0 1.0))
               (map (fn [x] (/ (- (f64 x) xsrng) xsrng)) xs))))");
    assert!(ir.contains("sitofp"));
    assert!(ir.contains("fdiv double"));
    assert!(ir.contains("fsub double"));
}

#[test]
fn filter_branches_on_i1() {
    let ir = emit(
        "(defn count-even-below [xs :- vec n :- i64] :- i64
           (reduce (fn [acc x] (if (< x n) (+ acc 1) acc)) 0 xs))");
    assert!(ir.contains("br i1"));
}

#[test]
fn capture_is_typed_from_param() {
    // n is a loop capture and must carry as i64 (from the param), not double.
    let ir = emit(
        "(defn count-even-below [xs :- vec n :- i64] :- i64
           (reduce (fn [acc x] (+ acc (* x n))) 0 xs))");
    assert!(ir.contains("i64 %n"));
    assert!(!ir.contains("double %n"));
}

#[test]
fn f64_comparison_uses_fcmp() {
    let ir = emit("(defn f [a :- f64 b :- f64] :- bool (< a b))");
    assert!(ir.contains("fcmp olt double"));
}

#[test]
fn min_max_use_select() {
    let ir = emit("(defn f [a :- i64 b :- i64] :- i64 (min a b))");
    assert!(ir.contains("icmp slt i64"));
    assert!(ir.contains("select i1"));
}
```

Update the two existing even-? tests (`emits_loop_with_even_filter_and_square_map`, `even_predicate_inverts_low_bit`) to the new `i1` emission: keep `and i64`, replace `icmp ne i64` with `icmp eq i64` (predicate now branches on the `i1`), and drop the `assert!(ir.contains("xor i64"))` from `even_predicate_inverts_low_bit` (rename it to reflect the new shape, e.g. `reads_even_predicate_low_bit_set`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test codegen_test`
Expected: FAIL — new assertions unmet, existing even-? assertions stale.

- [ ] **Step 3: Implementation**

**Block tracking.** Add to `FnCtx`:

```rust
struct FnCtx {
    counter: usize,
    blk: usize,
    cur: String,            // current basic-block label ("" if none)
    lines: Vec<String>,
    scalar: HashMap<String, (String, Ty)>,
    vecs: HashMap<String, (String, String)>,
}
impl FnCtx {
    fn block_name(&mut self) -> String { let n = self.blk; self.blk += 1; format!("%b{}", n) }
    fn open(&mut self, label: &str) -> String { self.cur = label.to_string(); label.to_string() }
}
```

In `emit_loop`, call `c.open("entry")` … `c.open("loop")`, `c.open("apply")`, `c.open("skip")`, `c.open("merge")`, `c.open("ret")` wherever the hardcoded labels are emitted. In `emit_plain`, `c.open("entry")` first. In the closure arm's `scratch`, set `scratch.cur = "%loop".to_string()` before emitting the body. `clone_with_lines` must copy `counter`, `blk`, `cur`, `scalar`, `vecs`.

**`emit_typed`** dispatch:

```rust
fn emit_typed(e: &HExpr, ty: Ty, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String,(Ty,Vec<Ty>)>) -> String {
    match ty {
        Ty::F64  => emit_float(e, c, mdl, sigs),
        Ty::Bool => emit_bool(e, c, mdl, sigs),
        Ty::I64  => emit_int(e, c, mdl, sigs),
        Ty::VecI64 => panic!("codegen: control flow cannot produce a vector"),
    }
}
```

**`emit_bool`** (new):

```rust
fn emit_bool(e: &HExpr, c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String,(Ty,Vec<Ty>)>) -> String {
    match e {
        HExpr::Cmp(op, a, b, is_f64) => {
            if *is_f64 {
                let av = emit_float(a, c, mdl, sigs);
                let bv = emit_float(b, c, mdl, sigs);
                let op = match op { CmpOp::Eq => "oeq", CmpOp::Ne => "one", CmpOp::Lt => "olt",
                                    CmpOp::Gt => "ogt", CmpOp::Le => "ole", CmpOp::Ge => "oge" };
                let t = c.temp();
                c.line(&format!("  {} = fcmp {} double {}, {}", t, op, av, bv));
                t
            } else {
                let av = emit_int(a, c, mdl, sigs);
                let bv = emit_int(b, c, mdl, sigs);
                let op = match op { CmpOp::Eq => "eq", CmpOp::Ne => "ne", CmpOp::Lt => "slt",
                                    CmpOp::Gt => "sgt", CmpOp::Le => "sle", CmpOp::Ge => "sge" };
                let t = c.temp();
                c.line(&format!("  {} = icmp {} i64 {}, {}", t, op, av, bv));
                t
            }
        }
        HExpr::And(a, b) => {
            let la = emit_bool(a, c, mdl, sigs);
            let parent = c.cur.clone();
            let rhs = c.block_name();
            let merged = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", la, rhs, merged));
            c.line(&format!("%{}:", c.open(&rhs)));
            let rb = emit_bool(b, c, mdl, sigs);
            c.line(&format!("  br label %{}", merged));
            c.line(&format!("%{}:", c.open(&merged)));
            let res = c.temp();
            c.line(&format!("  {} = phi i1 [ false, %{} ], [ {}, %{} ]", res, parent, rb, rhs));
            res
        }
        HExpr::Or(a, b) => {
            let la = emit_bool(a, c, mdl, sigs);
            let parent = c.cur.clone();
            let rhs = c.block_name();
            let merged = c.block_name();
            c.line(&format!("  br i1 {}, label %{}, label %{}", la, merged, rhs));
            c.line(&format!("%{}:", c.open(&rhs)));
            let rb = emit_bool(b, c, mdl, sigs);
            c.line(&format!("  br label %{}", merged));
            c.line(&format!("%{}:", c.open(&merged)));
            let res = c.temp();
            c.line(&format!("  {} = phi i1 [ true, %{} ], [ {}, %{} ]", res, parent, rb, rhs));
            res
        }
        HExpr::Not(v) => {
            let iv = emit_bool(v, c, mdl, sigs);
            let t = c.temp();
            c.line(&format!("  {} = xor i1 {}, true", t, iv));
            t
        }
        HExpr::Var(v) => {
            let (r, _) = c.scalar.get(v).unwrap_or_else(|| panic!("codegen: unknown bool var {}", v));
            r.clone()
        }
        HExpr::If(cond, then, els, Ty::Bool) => emit_if_node(cond, then, els, Ty::Bool, c, mdl, sigs),
        HExpr::Call(name, args) if name == "even?" => {
            let v = emit_int(&args[0], c, mdl, sigs);
            let p = c.temp();
            c.line(&format!("  {} = and i64 {}, 1", p, v));
            let t = c.temp();
            c.line(&format!("  {} = icmp eq i64 {}, 0", t, p));
            t
        }
        other => panic!("codegen: expected a boolean expression, got {:?}", other),
    }
}
```

In the `emit_int` `Call` arm, **delete** the `if name == "even?"` special-case (`even?` is boolean now; in an int context it falls to `call_user` and errors).

**`emit_if_node`** (used from `emit_int`, `emit_float`, `emit_bool` via `emit_typed`):

```rust
fn emit_if_node(cond: &HExpr, then: &HExpr, els: &HExpr, ty: Ty,
                c: &mut FnCtx, mdl: &mut Mdl, sigs: &HashMap<String,(Ty,Vec<Ty>)>) -> String {
    let lc = emit_bool(cond, c, mdl, sigs);
    let bt = c.block_name();
    let be = c.block_name();
    let bm = c.block_name();
    c.line(&format!("  br i1 {}, label %{}, label %{}", lc, bt, be));
    c.line(&format!("%{}:", c.open(&bt)));
    let tv = emit_typed(then, ty, c, mdl, sigs);
    c.line(&format!("  br label %{}", bm));
    c.line(&format!("%{}:", c.open(&be)));
    let ev = emit_typed(els, ty, c, mdl, sigs);
    c.line(&format!("  br label %{}", bm));
    c.line(&format!("%{}:", c.open(&bm)));
    let res = c.temp();
    let lty = if ty == Ty::F64 { "double" } else if ty == Ty::Bool { "i1" } else { "i64" };
    c.line(&format!("  {} = phi {} [ {}, %{} ], [ {}, %{} ]", res, lty, tv, bt, ev, be));
    res
}
```

Wire `If` into `emit_int`/`emit_float`:
`HExpr::If(cond, then, els, t) => emit_if_node(cond, then, els, *t, c, mdl, sigs)`.

**Abs/Min/Max int & float arms** (in `emit_int` and `emit_float`):

```rust
// emit_int
HExpr::Abs(e, _) => {
    let v = emit_int(e, c, mdl, sigs);
    let neg = c.temp(); c.line(&format!("  {} = sub i64 0, {}", neg, v));
    let lt  = c.temp(); c.line(&format!("  {} = icmp slt i64 {}, 0", lt, v));
    let t   = c.temp(); c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, neg, v));
    t
}
HExpr::Min(a, b, _) => {
    let av = emit_int(a, c, mdl, sigs); let bv = emit_int(b, c, mdl, sigs);
    let lt = c.temp(); c.line(&format!("  {} = icmp slt i64 {}, {}", lt, av, bv));
    let t  = c.temp(); c.line(&format!("  {} = select i1 {}, i64 {}, i64 {}", t, lt, av, bv));
    t
}
HExpr::Max(a, b, _) => { /* same with icmp sgt */ }

// emit_float
HExpr::Abs(e, _) => {
    let v = emit_float(e, c, mdl, sigs);
    let neg = c.temp(); c.line(&format!("  {} = fneg double {}", neg, v));
    let lt  = c.temp(); c.line(&format!("  {} = fcmp olt double {}, 0.000000e+00", lt, v));
    let t   = c.temp(); c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, neg, v));
    t
}
HExpr::Min(a, b, _) => { let lt = c.temp(); c.line(&format!("  {} = fcmp olt double {}, {}", lt, av, bv));
                         let t = c.temp(); c.line(&format!("  {} = select i1 {}, double {}, double {}", t, lt, av, bv)); t }
HExpr::Max(a, b, _) => { /* same with fcmp ogt */ }
```

**`emit_let`** — add a `Ty::Bool` arm: `let b = emit_bool(value, c, mdl, sigs); c.scalar.insert(name.clone(), (b, Ty::Bool));`.

**Typed map stages + closure-backed pipelines** in `emit_loop`:

First add a structural guard that routes any closure reduce whose *body* (or stage) can fork into blocks into the structured `apply/skip/merge` shape — the single-block scratch path cannot be used then because the combine result register would live in an inner block, not `%loop`:

```rust
fn has_ctl(e: &HExpr) -> bool {
    match e {
        HExpr::If(..) | HExpr::And(..) | HExpr::Or(..) | HExpr::Not(..) => true,
        HExpr::Bin(_, a, b) => has_ctl(a) || has_ctl(b),
        HExpr::ConvF64(x) | HExpr::Count(x) | HExpr::Abs(x, _) => has_ctl(x),
        HExpr::Call(_, args) => args.iter().any(has_ctl),
        HExpr::Let(binds, body) => binds.iter().any(|(_, e, _)| has_ctl(e)) || has_ctl(body),
        HExpr::Seq(vs) => vs.iter().any(has_ctl),
        HExpr::Cmp(..) => false,
        HExpr::Reduce(r) => match &**r {
            HReduce::Pipeline { init, maps, filters, src, .. } => has_ctl(init) || has_ctl(src)
                || maps.iter().any(|m| has_ctl(&m.body)) || filters.iter().any(|f| has_ctl(&f.body)),
            HReduce::Closure { init, src, body, maps, filters, .. } => has_ctl(init) || has_ctl(src)
                || has_ctl(body) || maps.iter().any(|m| has_ctl(&m.body))
                || filters.iter().any(|f| has_ctl(&f.body)),
        },
        _ => false,
    }
}
```

**Route** the `emit_loop` match on the reduce:
- `HReduce::Pipeline` with no maps/filters → existing single-block arm (unchanged — a binop combine can never fork).
- `HReduce::Closure` with **no maps, no filters, and `!has_ctl(body)`** → existing single-block scratch arm (unchanged — this covers `sum-of-squares`).
- Everything else (any maps, any filters, or a closure body containing control flow) → the structured `apply/skip/merge` arm.

**Structured arm** (phi predecessors taken from `c.cur`, never hardcoded labels):

```rust
c.line("");
c.line("loop:");
c.open("loop");
c.line("  %i = phi i64 [ 0, %entry ], [ %i.next, %merge ]");
c.line(&format!(
    "  %acc = phi {} [ {}, %entry ], [ %acc.merge, %merge ]",
    acc_ll, init_lit
));
c.line("  %p = getelementptr inbounds i64, ptr %data, i64 %i");
c.line("  %x = load i64, ptr %p, align 8");
c.scalar.insert(el.clone(), ("%x".to_string(), Ty::I64));

if has_filters {
    for fl in filters {
        c.scalar.insert(fl.el.clone(), ("%x".to_string(), Ty::I64));
        let pv = emit_bool(&fl.body, c, mdl, sigs);
        let t_keep = c.temp();
        c.line(&format!("  {} = icmp ne i1 {}, 0", t_keep, pv));
        c.line(&format!("  br i1 {}, label %apply, label %skip", t_keep));
    }
} else {
    c.line("  br label %apply");
}

c.line("");
c.line("apply:");
c.open("apply");
let mut val = "%x".to_string();
let mut val_ty = Ty::I64;
for m in maps {
    c.scalar.insert(m.el.clone(), (val.clone(), m.el_ty));
    val = emit_typed(&m.body, m.res_ty, c, mdl, sigs);
    val_ty = m.res_ty;
}
c.scalar.insert(accsym.clone(), ("%acc".to_string(), spec.acc_ty));
let r = match &spec.reduce {
    HReduce::Closure { body, .. } => {
        c.scalar.insert(el.clone(), (val.clone(), val_ty));
        emit_typed(body, spec.acc_ty, c, mdl, sigs)
    }
    HReduce::Pipeline { combine, .. } => {
        let t = c.temp();
        c.line(&format!("  {} = {}", t, combine_step(*combine, &val, "%acc", spec.acc_ty, &mut c)));
        t
    }
    _ => unreachable!(),
};
let r = pin_value(acc_ll, &r, &mut c); // pins a bare literal into a temp *before* the br
let apply_pred = c.cur.clone();        // the block the incoming edge leaves
c.line("  br label %merge");

c.line("");
c.line("skip:");
c.open("skip");
c.line("  br label %merge");

c.line("");
c.line("merge:");
c.open("merge");
c.line(&format!(
    "  %acc.merge = phi {} [ {}, %{} ], [ %acc, %skip ]",
    acc_ll, r, apply_pred
));
c.line("  %i.next = add i64 %i, 1");
carried = "%acc.merge".to_string();
carried_block = "%merge";
let t_cond = c.temp();
c.line(&format!("  {} = icmp ult i64 %i.next, %len", t_cond));
c.line(&format!("  br i1 {}, label %loop, label %ret", t_cond));
```

`pin_value` (emits its instruction in the current block, i.e. `apply_pred`):

```rust
fn pin_value(ll_ty: &str, r: &str, c: &mut FnCtx) -> String {
    if r.starts_with('%') {
        return r.to_string();
    }
    let t = c.temp();
    if ll_ty == "double" {
        c.line(&format!("  {} = fadd double {}, 0.0", t, r));
    } else {
        c.line(&format!("  {} = add i64 {}, 0", t, r));
    }
    t
}
```

The predecessor rule is what matters: every phi operand's predecessor is `c.cur` captured right before the controlling `br` is emitted (`apply_pred` above, and `parent` in `emit_bool`'s `And`/`Or`). When a closure body forks (`if`/`and`), the body result and the phi operand live in the same last inner block, and it is that block — not `%apply` — that must appear in the phi. The existing `filter` emission switches from `emit_int`/`icmp ne i64` to the `emit_bool` + `icmp ne i1` form shown above.

**Capture typing from params.** `make_loop_spec` gains `params: &[(String, Ty)]`; `rewrite_body` gains `params: &[(String, Ty)]` (threaded through from `emit_module`, which passes `d.params`). Cap type resolution:

```rust
let mut caps: Vec<(String, Ty)> = free.vars
    .drain(..)
    .map(|v| {
        let t = params.iter().find(|(n, _)| *n == v).map(|(_, t)| *t).unwrap_or(Ty::F64);
        (v, t)
    })
    .collect();
caps.sort_by(|a, b| a.0.cmp(&b.0));
```

`emit_loop`'s cap parameter emission uses `bool → "i1"`, `f64 → "double"`, else `"i64"`.

`make_loop_spec` has **two call sites**; both must pass the enclosing defn's params:
- `emit_module` top-level loop defns: `make_loop_spec(&mangle(&d.name), reduce, &d.params)`.
- `rewrite_body`'s `Reduce` arm: `make_loop_spec(&helper, (**r).clone(), params)` — same `params` given to `rewrite_body` (which `emit_module` passes as `&d.params`).

**Free-variable collection must cover the new closure stages.** `loop_free_vars`' `Closure` arm and `collect_vars`' `Closure` arm currently only walk `init`/`src`/`body`; add the same `maps`/`filters` pass the `Pipeline` arm already does (insert each stage body, `.remove` its element symbol), or a `xsrng`-style capture referenced only inside a map stage will be dropped and codegen will emit an undefined `%xsrng`.

**`bool` routing in the plain-function path** (Task 3): `emit_for_ret` must route `Ty::Bool` to `emit_bool`; `emit_plain`'s param-type `match` gets a `Ty::Bool` arm emitting `i1 %name` and inserting `scalar (mangle(name), Bool)`; `call_user` gets a `Ty::Bool` arg arm (emit via `emit_bool`, arg text `i1 <v>`) and `ret_ll` for bool callees = `"i1"`. These replace the Task 2 stub arms.

**`emit_float` `Abs`** uses `llvm_f64(0.0)` (helper produces `0.0e0`) for the `fcmp olt double <v>, <llvm_f64(0.0)>` compare, not a hand-written literal.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS (all codegen tests, hir, fuse, integration, and the two updated even-? tests).

- [ ] **Step 5: Commit**

```bash
git add src/codegen.rs src/fuse.rs tests/codegen_test.rs
git commit -m "feat: codegen bool context, if/and/or phis, typed pipeline stages, param captures"
```

---

### Task 4: Reference Programs & End-to-End Tests

**Files:**
- Create: `examples/normalize.cato`
- Modify: `tests/integration.rs`

- [ ] **Step 1: Write the failing test** — append to `tests/integration.rs`:

```rust
fn compile_and_run(src_name: &str, src: &str) -> String {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_path = manifest.join("target").join(src_name);
    std::fs::write(&src_path, src).expect("write source");
    let out_ll = manifest.join("target").join(format!("{}.ll", src_name));
    let rt = manifest.join("runtime/runtime.c");
    let exe = manifest.join("target").join(format!("{}_bin", src_name));

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src_path).arg("-o").arg(&out_ll)
        .status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    assert!(out.status.success(), "binary failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn compiles_normalize_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("examples/normalize.cato")).unwrap();
    assert_eq!(compile_and_run("normalize", &src), "0.75\n");
}

#[test]
fn compiles_count_even_below_to_expected_output() {
    let src = r#"
(defn count-even-below [xs :- vec n :- i64] :- i64
  (reduce (fn [acc x] (if (and (even? x) (< x n)) (+ acc 1) acc)) 0 xs))
(defn main []
  (print-i64 (count-even-below (vec-i64 1 2 3 4 5 6) 6)))
"#;
    assert_eq!(compile_and_run("count_even_below", &src), "2\n");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration`
Expected: FAIL — `examples/normalize.cato` missing; `count-even-below` wrong/missing output.

- [ ] **Step 3: Implementation**

`examples/normalize.cato`:

```clojure
; normalize.cato — spec §5 normalize example, written with the ->> macro.
;
; (->> xs (map f) (map g) (reduce r 0.0)) expands to
; (reduce r 0.0 (map g (map f xs))), which stream-fuses into ONE loop.

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

Expected trace (all exact dyadic values): stage 1 → (2−8)/8 = −0.75, −0.5, −0.25; stage 2 unaffected (all inside [−1,1]); reduce → max(−0.75|, |−0.5|, |−0.25|) over init 0.0 = 0.75 → `printf("%g")` prints `0.75`.

Note: `count-even-below` exercises exactly the Task 3 structured arm — a closure reduce whose `body` contains `if` + `and` + a comparison, with an `i64` loop capture (`n` — typed from the param) — and with `(vec-i64 1 2 3 4 5 6)` and `n = 6` it prints `2`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS. Also verify manually:

```bash
cargo run -- examples/normalize.cato -o /tmp/normalize.ll
clang /tmp/normalize.ll runtime/runtime.c -o /tmp/normalize_bin
/tmp/normalize_bin            # -> 0.75
cargo run -- examples/slice.cato -o /tmp/slice.ll
clang /tmp/slice.ll runtime/runtime.c -o /tmp/slice_bin
/tmp/slice_bin                # -> 2.5 / 20 (unchanged)
```

- [ ] **Step 5: Commit**

```bash
git add examples/normalize.cato tests/integration.rs
git commit -m "feat: normalize reference program + control-flow e2e tests"
```

---

## Self-Review

**Spec coverage (control-flow and macros design):**
- `->`/`->>` macro stage → Task 1. ✓
- `Ty::Bool`, `CmpOp`, `If`/`And`/`Or`/`Not`/`Cmp`/`Abs`/`Min`/`Max` lowering + branch promotion → Task 2. ✓
- Pipeline-backed closures (closure reduce over `map`/`filter` source) → Task 2 lower + Task 3 emit. ✓
- `MapSpec.el_ty`/`res_ty`, `FilterSpec.el_ty` → Task 2; f64 map stage `sitofp`, `i1` filters → Task 3. ✓
- `even?` → bool (`i1`) → Task 3 (emission) with type change at Task 2 (`ty_of`). ✓
- Capture typing from defn params → Task 3. ✓
- `examples/normalize.cato` → `0.75\n`; `count-even-below` → `2\n` → Task 4. ✓
- `noalias`/`inbounds`/`align 8` preserved (loop header and params untouched). ✓
- Out of scope recorded in spec §2 (polymorphism, ownership, `deftype`/`match`, strings/maps, other macros, `take`/`drop`/`zip`/`range`). ✓

**Compiler-greenness per task:**
- Task 1 touches only `expand.rs`/`lib.rs`/`main.rs` — no HIR impact. ✓
- Task 2 exhaustively updates `fuse.rs` matches and stubs codegen emit arms so the crate compiles and old tests stay green; new nodes only arise from new lowering, which no existing test feeds to codegen. ✓ (Stubs panic loudly if accidentally reached.)
- Task 3 fills real emission and updates the two even-? assertion tests. ✓
- Task 4 only adds examples + integration. ✓

**Phi-predicate correctness in Task 3:** every phi predecessor is `c.cur` captured at the moment the controlling `br` is emitted — never a hardcoded label. A closure reduce whose `body` (or a stage) forks into blocks must use the structured `apply/skip/merge` arm (guard: `has_ctl`), because a fork moves the combine result into an inner block whose label is the true predecessor. The single-block scratch arm is unchanged and only ever emits block-free bodies, so existing `sum-of-squares` IR stays byte-identical.

**Type consistency:** `Expr` (`ast`) → `Vec<Expr>` (`expand::expand_all`) → `Vec<Defn>` (`hir::build`) → fused (`fuse::check`) → `emit_module(&[Defn])`. The `Ty`-annotated `If`/`Abs`/`Min`/`Max`/`Cmp(f64 flag)` mean codegen never re-infers control-flow types. `MapSpec.el_ty`/`res_ty` are computed in `pipeline_src` (threaded into `lambda_spec` so stage bodies type-check against their real element type) and consumed directly by `emit_loop`. Capture types resolve from the enclosing defn's `params` (fallback `f64`); `loop_free_vars`/`collect_vars` cover `Closure` `maps`/`filters` so `xsrng`-style captures survive.

**Placeholder scan:** every task has concrete code, file paths, exact test bodies, and commands; no TBD/TODO. The two manually-computed reference outputs (0.75 and 2) are derivations in §Task 4 and the spec. The two existing `even?` codegen tests are updated in Task 3, not silently red.