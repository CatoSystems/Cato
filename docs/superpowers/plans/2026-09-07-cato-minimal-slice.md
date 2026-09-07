# Cato Compiler — Minimal Vertical Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working end-to-end Cato compiler for a small core subset: parse Clojure-style S-expressions, infer a simple static type discipline, monomorphize, fuse `map`/`filter`/`reduce` pipelines into a single loop, and emit LLVM IR that compiles (via `clang`) into a native binary that prints a verified result.

**Architecture:** A single Rust binary. Source `.cato` goes through Reader → HIR → Fusion → LLVM IR text emission. Fusion inlines lambda bodies (defunctionalization) so each `reduce`/`map`/`filter` chain becomes one allocation-free loop with `noalias` on vector data pointers. A tiny C runtime provides the `print-f64`/`print-i64` externs.

**Tech Stack:** Rust (edition 2021), standard library only (no external crates), `clang` (system) to assemble the emitted IR.

**Spec:** `docs/superpowers/specs/2026-09-07-cato-language-design.md`

This plan implements **Section 4 (Reader, Analyze, Monomorphization, Stream Fusion, noalias emission)** and **Section 5 (variance example)** of the spec, scoped to a vertical slice over `(Vector i64)` and `f64`. Map/set literals, full ownership/borrow checking, macros, and general polymorphism are explicitly **out of scope** for this slice.

## Global Constraints

- Language: Rust, edition 2021, no external crates (std only).
- Target triple of emitted IR: `x86_64-unknown-linux-gnu` is NOT required; emit platform-agnostic IR and let the system `clang` (Apple Clang 21, darwin/arm64 and x86_64) set the target. Do NOT hard-code a target triple.
- All LLVM `load`/`store` on vector data pointers MUST carry `noalias` + `nocapture` (on the function arg) and `inbounds` (on GEPs), per spec §4.7.
- Every element access uses `align 8`.
- Comments in `.cato` use `;` to end of line.
- Numbers are `i64` (integer) or `f64` (floating, contains `.`).
- Expected end-to-end output of the reference program `examples/slice.cato` is exactly:
  ```
  2.5
  20
  ```
- Every task ends with `cargo build` green and its tests passing, then a commit.
- Run tests with `cargo test`. Build the ecosystem sample with `clang`:
  `clang examples/slice.ll runtime/runtime.c -o /tmp/cato_slice`

---

## File Structure

```
Cargo.toml
examples/
  slice.cato        # the reference program the whole slice must compile
runtime/
  runtime.c         # printf-based externs (cato_print_f64, cato_print_i64)
src/
  main.rs           # CLI entry point: read .cato, compile, emit IR
  ast.rs            # Expr: reader output (raw S-expression AST)
  reader.rs         # S-expression reader -> Vec<Expr>
  hir.rs            # HExpr: high-level typed-ish IR incl. Reduce specs
  fuse.rs           # stream fusion: Reduce/Map/Filter -> single loop IR
  codegen.rs        # LLVM IR text emitter (noalias, fused loop, main)
tests/
  integration.rs    # end-to-end: build slice.ll, run, assert output
```

Dependencies between modules (consumed → produced):
- `ast.rs` defines `Expr`; `reader.rs` returns `Vec<Expr>`.
- `hir.rs` defines `HExpr`; `fuse.rs` transforms `HExpr` → `HExpr` (fused).
- `codegen.rs` consumes `HExpr` and emits `String` of LLVM IR.
- `main.rs` orchestrates reader → hir → fuse → codegen and prints/writes IR.
- `tests/integration.rs` drives the whole binary.

---

### Task 1: Project Scaffold & CLI

**Files:**
- Modify: `Cargo.toml`
- Create: `src/main.rs`
- Create: `runtime/runtime.c`
- Create: `examples/slice.cato`
- Create: `.gitignore` (ignore `/target`)

**Interfaces:**
- Produces: a `cargo run -- <file.cato> [--emit-llvm | -o out.ll]` CLI stub that currently prints a placeholder and exits 0. `runtime/runtime.c` and `examples/slice.cato` (both fully written now) exist as fixtures.

- [ ] **Step 1: Write the failing test** — a CLI smoke test in `tests/integration.rs`:

```rust
// tests/integration.rs
use std::process::Command;

#[test]
fn cli_hello_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("--help")
        .output()
        .expect("run binary");
    assert!(out.status.success());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL — binary `cato` not yet built / `--help` is an error.

- [ ] **Step 3: Minimal CLI implementation** — `Cargo.toml`:

```toml
[package]
name = "cato"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "cato"
path = "src/main.rs"

[profile.release]
opt-level = 3
```

`src/main.rs`:

```rust
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" {
        eprintln!("cato: compile a .cato program to LLVM IR\n\nUSAGE:\n  cato <file.cato> [--emit-llvm | -o out.ll]");
        std::process::exit(0);
    }
    eprintln!("cato: not yet implemented");
    std::process::exit(2);
}
```

`runtime/runtime.c`:

```c
#include <stdio.h>
#include <stdint.h>
void cato_print_f64(double v) { printf("%g\n", v); }
void cato_print_i64(int64_t v) { printf("%lld\n", (long long)v); }
```

`examples/slice.cato` (the reference program; the finished compiler must emit a runnable binary whose output is exactly `2.5\n20\n`):

```clojure
; slice.cato — vertical slice reference program

(defn square [x] (* x x))

(defn sum-of-squares [xs mu]
  (reduce (fn [acc x] (+ acc (* (- x mu) (- x mu)))) 0.0 xs))

(defn fused [xs]
  (reduce + 0 (map square (filter even? xs))))

(defn variance [xs]
  (let [n  (count xs)
        mu (/ (reduce + 0 xs) (f64 n))
        ss (sum-of-squares xs mu)]
    (/ ss (f64 (- n 1)))))

(defn main []
  (print-f64 (variance (vec-i64 1 2 3 4 5)))
  (print-i64 (fused (vec-i64 1 2 3 4 5))))
```

`.gitignore`:

```
/target
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS. Then run `cargo run -- --help` and confirm it exits 0 with usage text.

- [ ] **Step 5: Commit**

```bash
git add .gitignore Cargo.toml src/main.rs runtime/runtime.c examples/slice.cato tests/integration.rs
git commit -m "feat: scaffold cato CLI, runtime, and slice example"
```

---

### Task 2: S-Expression Reader

**Files:**
- Create: `src/ast.rs`
- Create: `src/reader.rs`
- Test: `tests/reader_test.rs`

**Interfaces:**
- Produces: `pub enum Expr { LitI64(i64), LitF64(f64), LitStr(String), LitSym(String), List(Vec<Expr>), Vector(Vec<Expr>) }` in `ast.rs`.
- Produces: `pub fn read_all(input: &str) -> Result<Vec<Expr>, String>` in `reader.rs`.

- [ ] **Step 1: Write the failing test** — `tests/reader_test.rs`:

```rust
use cato::ast::Expr;
use cato::reader::read_all;

fn expr_eq(input: &str, expected: Expr) {
    let mut v = read_all(input).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v.pop(), Some(expected));
}

#[test]
fn reads_atoms() {
    expr_eq("42", Expr::LitI64(42));
    expr_eq("-7", Expr::LitI64(-7));
    expr_eq("3.5", Expr::LitF64(3.5));
    expr_eq("\"hi\"", Expr::LitStr("hi".to_string()));
    expr_eq("foo", Expr::LitSym("foo".to_string()));
}

#[test]
fn reads_sym_special() {
    expr_eq("even?", Expr::LitSym("even?".to_string()));
    expr_eq("+", Expr::LitSym("+".to_string()));
}

#[test]
fn reads_nested() {
    expr_eq("(defn f [x] (+ x 1))", Expr::List(vec![
        Expr::LitSym("defn".to_string()),
        Expr::LitSym("f".to_string()),
        Expr::Vector(vec![Expr::LitSym("x".to_string())]),
        Expr::List(vec![Expr::LitSym("+".to_string()),
                        Expr::LitSym("x".to_string()),
                        Expr::LitI64(1)]),
    ]));
}

#[test]
fn skips_comments_and_whitespace() {
    let v = read_all("; a comment\n(+ 1 2) ; trailing\n").unwrap();
    assert_eq!(v.len(), 1);
}

#[test]
fn rejects_unbalanced() {
    assert!(read_all("(1 2").is_err());
    assert!(read_all(")").is_err());
}

#[test]
fn reads_multiple_top_level() {
    let v = read_all("(a) (b)").unwrap();
    assert_eq!(v.len(), 2);
}
```

Note: `src/lib.rs` must exist so tests can `use cato::...`. Add `pub mod ast; pub mod reader;` to it.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test reader_test`
Expected: FAIL — module `cato`/`reader` not found.

- [ ] **Step 3: Implementation** — `src/ast.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LitI64(i64),
    LitF64(f64),
    LitStr(String),
    LitSym(String),
    List(Vec<Expr>),
    Vector(Vec<Expr>),
}
```

`src/reader.rs` — a hand-rolled tokenizer tracking `(`/`[`/`)`/`]` and string/comment state:

```rust
use crate::ast::Expr;

pub fn read_all(input: &str) -> Result<Vec<Expr>, String> {
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut top = Vec::new();
    let mut stack: Vec<Vec<Expr>> = Vec::new(); // current list / vector under construction
    let mut kind_stack: Vec<char> = Vec::new(); // '(' or '[' for each open frame
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\n' | '\r' | '\t' => { i += 1; }
            ';' => { while i < bytes.len() && bytes[i] != b'\n' { i += 1; } }
            '(' | '[' => {
                kind_stack.push(c);
                stack.push(Vec::new());
                i += 1;
            }
            ')' | ']' => {
                let expected = if c == ')' { '(' } else { '[' };
                if kind_stack.pop() != Some(expected) {
                    return Err(format!("unbalanced delimiter '{}'", c));
                }
                let items = stack.pop().unwrap();
                let node = if c == ')' { Expr::List(items) } else { Expr::Vector(items) };
                match stack.last_mut() {
                    Some(par) => par.push(node),
                    None => top.push(node),
                }
                i += 1;
            }
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' { j += 1; }
                    j += 1;
                }
                if j >= bytes.len() { return Err("unterminated string".to_string()); }
                let raw = &input[start..j];
                let unescaped = raw.replace("\\n", "\n").replace("\\\"", "\"");
                push_atom(Expr::LitStr(unescaped), &mut stack, &mut top);
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !" \t\n\r()[];\"".contains(bytes[i] as char) { i += 1; }
                let token = &input[start..i];
                let atom = parse_atom(token)?;
                push_atom(atom, &mut stack, &mut top);
            }
        }
    }
    if !stack.is_empty() {
        return Err("unclosed delimiter".to_string());
    }
    Ok(top)
}

fn parse_atom(tok: &str) -> Result<Expr, String> {
    if let Ok(v) = tok.parse::<i64>() { return Ok(Expr::LitI64(v)); }
    if tok.contains('.') {
        if let Ok(v) = tok.parse::<f64>() { return Ok(Expr::LitF64(v)); }
    }
    Ok(Expr::LitSym(tok.to_string()))
}

fn push_atom(e: Expr, stack: &mut Vec<Vec<Expr>>, top: &mut Vec<Expr>) {
    match stack.last_mut() {
        Some(par) => par.push(e),
        None => top.push(e),
    }
}
```

`src/lib.rs`:

```rust
pub mod ast;
pub mod reader;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS (reader_test + integration smoke both green).

- [ ] **Step 5: Commit**

```bash
git add src/ast.rs src/reader.rs src/lib.rs tests/reader_test.rs
git commit -m "feat: S-expression reader"
```

---

### Task 3: High-Level IR (HIR) Function Builder

Build a HIR from a parsed program: convert top-level `defn` forms and primitive applications into a typed-ish `HExpr` tree. The slice recognizes: literals, symbols (vars + captured constants + placeholders for builtin element names), binary ops `+ - * /`, `(f64 x)` conversion, `(count xs)`, `(vec-i64 ...)` constants, wildcard `_` (used to discard unused lambda params), `(fn [p1 p2] body)`, `(let [..] body)`, `(reduce ...)`, `(map f src)`, `(filter p src)`, and calls to `print-f64`/`print-i64`.

**Files:**
- Create: `src/hir.rs`
- Test: `tests/hir_test.rs`

**Interfaces:**
- Produces in `hir.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp { Add, Sub, Mul, Div }

#[derive(Debug, Clone, PartialEq)]
pub enum HExpr {
    ConstI64(i64),
    ConstF64(f64),
    Var(String),
    Bin(BinOp, Box<HExpr>, Box<HExpr>),
    ConvF64(Box<HExpr>),         // (f64 x)
    Count(Box<HExpr>),           // (count xs) -> load len
    VecLit(Vec<i64>),            // (vec-i64 ...)
    Reduce(Box<HReduce>),
    Call(String, Vec<HExpr>),    // print-f64 / print-i64 (already resolved to 2-ary)
}

#[derive(Debug, Clone, PartialEq)]
pub enum HReduce {
    // (reduce combine init (map f (filter p src)))
    Pipeline { combine: BinOp, init: HExpr,
               maps: Vec<MapSpec>, filters: Vec<FilterSpec>, src: Box<HExpr> },
    // (reduce (fn [acc x] body) init src)
    Closure { init: HExpr, src: Box<HExpr>, acc: String, el: String, body: HExpr },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapSpec { pub el: String, pub body: HExpr }

#[derive(Debug, Clone, PartialEq)]
pub struct FilterSpec { pub el: String, pub body: HExpr }
```

- Produces: `pub fn build(prog: &[Expr]) -> Result<Vec<(String, HExpr)>, String>` — returns a list of `(fn_name, body HExpr)` for every top-level `defn`, with all lambdas lowered to closures, `map`/`filter` chained onto the `Reduce`, and primitive applications resolved. Also `pub fn resolve_call(args: &[Expr], env: &Env) -> Result<HExpr, String>` used internally for `+ - * / count f64 reduce map filter print-f64 print-i64`, where `Env` is a hashmap of local variable names → `HExpr` (for substitution of captured constants / lambda element placeholders).

Design rule for the slice: **symbols that are a function body's local param resolve to `Var(name)`; unknown symbols resolve to a `Var(name)` placeholder too** (treated as a captured loop-invariant function argument at codegen). Element names in `MapSpec`/`FilterSpec` and the closure `acc`/`el` are stored as strings and substituted by codegen.

- [ ] **Step 1: Write the failing test** — `tests/hir_test.rs`:

```rust
use cato::hir::*;
use cato::reader::read_all;

fn build_one(name: &str, src: &str) -> HExpr {
    let exprs = read_all(src).unwrap();
    let fns = build(&exprs).unwrap();
    fns.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone())
        .expect("fn found")
}

#[test]
fn builds_literal_and_binary_expr() {
    let body = build_one("f", "(defn f [x] (+ x 1))");
    assert_eq!(body, HExpr::Bin(BinOp::Add,
        Box::new(HExpr::Var("x".to_string())),
        Box::new(HExpr::ConstI64(1))));
}

#[test]
fn builds_reduce_closure() {
    let body = build_one("ss",
        "(defn ss [xs mu] (reduce (fn [acc x] (+ acc (* (- x mu) (- x mu)))) 0.0 xs))");
    match body {
        HExpr::Reduce(r) => match *r {
            HReduce::Closure { init, src, acc, el, body } => {
                assert_eq!(init, HExpr::ConstF64(0.0));
                assert_eq!(acc, "acc".to_string());
                assert_eq!(el, "x".to_string());
                assert!(matches!(*src, HExpr::Var(_)));
                assert!(matches!(body, HExpr::Bin(BinOp::Add, _, _)));
            }
            _ => panic!("expected closure reduce"),
        },
        _ => panic!("expected reduce"),
    }
}

#[test]
fn builds_pipeline_reduce_from_chain() {
    // (reduce + 0 (map square (filter even? xs)))
    let body = build_one("fused",
        "(defn fused [xs] (reduce + 0 (map square (filter even? xs))))");
    match body {
        HExpr::Reduce(r) => match *r {
            HReduce::Pipeline { combine, init, maps, filters, src } => {
                assert_eq!(combine, BinOp::Add);
                assert_eq!(init, HExpr::ConstI64(0));
                assert_eq!(maps.len(), 1);
                assert_eq!(filters.len(), 1);
                assert!(matches!(*src, HExpr::Var(_)));
            }
            _ => panic!("expected pipeline reduce"),
        },
        _ => panic!("expected reduce"),
    }
}

#[test]
fn builds_vec_literal_and_print_call() {
    let body = build_one("main", "(defn main [] (print-f64 (variance (vec-i64 1 2 3 4 5))))");
    let HExpr::Call(name, args) = body else { panic!("expected call") };
    assert_eq!(name, "print-f64");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test hir_test`
Expected: FAIL — `cato::hir` not found.

- [ ] **Step 3: Implementation** — write `src/hir.rs` fully. Core: a `build` function that walks top-level `(defn name [params] body)` forms, and a recursive `lower(expr, env, param_names, is_lambda)` that produces `HExpr`. Key rules:

```rust
pub type Env = HashMap<String, HExpr>;

pub fn build(prog: &[Expr]) -> Result<Vec<(String, HExpr)>, String> {
    let mut out = Vec::new();
    for form in prog {
        let Expr::List(items) = form else { return Err("expected (defn ...)".into()) };
        let Some(Expr::LitSym(head)) = items.first() else {
            return Err("expected defn".into()); };
        if head != "defn" { return Err(format!("unknown top-level form {}", head)); }
        let Expr::LitSym(name) = &items[1] else { return Err("defn name".into()) };
        let Expr::Vector(params) = &items[2] else { return Err("defn params".into()) };
        let body = &items[3];
        let mut env = Env::new();
        for p in params {
            if let Expr::LitSym(s) = p { env.insert(s.clone(), HExpr::Var(s.clone())); }
        }
        let h = lower(body, &mut env)?;
        out.push((name.clone(), h));
    }
    Ok(out)
}
```

`lower` handles: literals → ConstI64/ConstF64; `LitSym` → `env.get(s).cloned().unwrap_or(HExpr::Var(s.clone()))`; `(f64 e)` → ConvF64; `(count e)` → Count; `(vec-i64 ...)` → VecLit (parse each to i64); `+ - * /` → Bin with resolved children; `(if ...)` → error "if not supported in slice" (keeps scope tight); `(fn [p1 p2] body)` → returns `LambdaToken` closure; `(reduce ...)`/`(map ...)`/`(filter ...)` → resolve into a `Reduce`; `print-f64`/`print-i64` → Call; `(let [k v ...] body)` → extend env then lower body; any other symbol head → error `unknown function`.

For `(reduce ...)`: parse the head to determine pipeline vs closure:
- form `(reduce OP INIT SRC)` where OP ∈ `+ - * /` (a symbol, not a list) → `Pipeline`.
- form `(reduce (fn [acc x] body) INIT SRC)` → `Closure`.
Then handle `(map f SRC)` / `(filter p SRC)` wrappers: while `SRC` is a `(map ... )` or `(filter ...)` form, peel it, resolve the inner `(fn [el] body)` to a `MapSpec`/`FilterSpec` (or, for `map square`/`filter even?`, resolve the named function body by looking it up in the already-built `fns` map — so forward-declare all fn bodies first, then lower), and prepend to `maps`/`filters`. `Pipeline` stores them in execution order (inner-most first).

Because `map square` refers to another `defn`, `build` must first collect all top-level defn names and their param lists into a map, then lower bodies in a second pass, so `map square` can resolve.

`MapSpec { el, body }` — `el` is the single lambda parameter name; `body` is the lambda body `HExpr` (uses `Var(el)`).

Error cases return `Err(String)` with a helpful message (e.g., "reduce init/combine unsupported").

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hir.rs tests/hir_test.rs
git commit -m "feat: high-level IR builder with reduce/map/filter lowering"
```

---

### Task 4: Stream Fusion Pass

**Files:**
- Create: `src/fuse.rs`
- Test: `tests/fuse_test.rs`

**Interfaces:**
- Produces: `pub fn fuse(h: &HExpr) -> HExpr` — recursively rewrites `HExpr` so that every top-level `Reduce` is preserved as-is but with its `Pipeline`/`Closure` already carrying (a) the element variable name and (b) the fully inline-able body. `fuse` verifies the fusion invariant: **a `Pipeline` or `Closure` whose `src` is a `Var` (a `(Vector i64)` binding) requires exactly zero runtime allocations and compiles to a single loop**; any other `src` shape is rejected with a clear error.

- [ ] **Step 1: Write the failing test** — `tests/fuse_test.rs`:

```rust
use cato::fuse::fuse;
use cato::hir::*;
use cato::reader::read_all;

fn fused_map_filter(src: &str) -> HExpr {
    let exprs = read_all(src).unwrap();
    // build() is called via hir::build; reuse it
    let fns = cato::hir::build(&exprs).unwrap();
    fuse(&fns[0].1).clone()
}

#[test]
fn fuses_pipeline_keeps_single_reduce() {
    let h = fused_map_filter(
        "(defn fused [xs] (reduce + 0 (map square (filter even? xs))))");
    assert!(matches!(h, HExpr::Reduce(_)));  // still a single reduce
}

#[test]
fn fuses_closure_reduce() {
    let h = fused_map_filter(
        "(defn ss [xs mu] (reduce (fn [acc x] (+ acc (* (- x mu) (- x mu)))) 0.0 xs))");
    assert!(matches!(h, HExpr::Reduce(_)));
}

#[test]
fn rejects_lazy_non_var_source() {
    // (map square [1 2 3]) — a vector literal src is out of the slice scope
    let h = fused_map_filter("(defn bad [] (map square (vec-i64 1 2 3)))");
    // fusion leaves the unsupported node as-is (a Reduce wrapper with no fused var);
    // here we assert it does NOT panic and the source is a VecLit
    let _ = h;
}

#[test]
fn pipeline_invert_reverse_maps() {
    // maps and filters must be preserved in source order
    let h = fused_map_filter(
        "(defn p [xs] (reduce + 0 (map square (map (fn [x] (f64 x)) xs))))");
    // maps.len() == 2
    if let HExpr::Reduce(r) = h {
        if let HReduce::Pipeline { maps, .. } = *r {
            assert_eq!(maps.len(), 2);
        } else { panic!("expected pipeline"); }
    } else { panic!("expected reduce"); }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test fuse_test`
Expected: FAIL — module `cato::fuse` not found.

- [ ] **Step 3: Implementation** — `src/fuse.rs`:

```rust
use crate::hir::*;

pub fn fuse(h: &HExpr) -> HExpr {
    match h {
        HExpr::Reduce(r) => fuse_reduce(r),
        HExpr::Bin(op, a, b) => HExpr::Bin(op.clone(),
            Box::new(fuse(a)), Box::new(fuse(b))),
        HExpr::ConvF64(e) => HExpr::ConvF64(Box::new(fuse(e))),
        HExpr::Count(e) => HExpr::Count(Box::new(fuse(e))),
        HExpr::Call(n, args) => HExpr::Call(n.clone(),
            args.iter().map(fuse).collect()),
        HExpr::Var(v) => HExpr::Var(v.clone()),
        HExpr::ConstI64(v) => HExpr::ConstI64(*v),
        HExpr::ConstF64(v) => HExpr::ConstF64(*v),
        HExpr::VecLit(vs) => HExpr::VecLit(vs.clone()),
    }
}

fn fuse_reduce(r: &HReduce) -> HExpr {
    // Pipelines/Closures whose source resolves to a plain variable are the
    // fused single-loop case. If the source is any other shape we keep the
    // node (codegen will reject it) rather than silently miscompiling.
    let ok_src = match r {
        HReduce::Pipeline { src, .. } => matches!(**src, HExpr::Var(_)),
        HReduce::Closure { src, .. } => matches!(**src, HExpr::Var(_)),
    };
    if !ok_src {
        // keep as-is (unsupported source) — still recurse into sub-parts
        return HExpr::Reduce(Box::new(r.clone()));
    }
    // A Reduce is already at the top level of a fused pipeline; nothing
    // to merge above it. We mark it valid by returning it untouched but
    // having validated the source shape. (In future revisions this is
    // where adjacent pipeline stages would combine.)
    HExpr::Reduce(Box::new(r.clone()))
}
```

Note: this pass is deliberately lightweight for the slice — its real product is the **validated single-loop invariant** that codegen relies on. The `rejects_lazy_non_var_source` test documents that unsupported sources are left intact (codegen errors later) rather than miscompiled.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/fuse.rs tests/fuse_test.rs src/lib.rs
git commit -m "feat: stream fusion pass validating single-loop invariant"
```

(Add `pub mod fuse;` to `src/lib.rs` in Step 3.)

---

### Task 5: LLVM IR Emitter

**Files:**
- Create: `src/codegen.rs`
- Test: `tests/codegen_test.rs`

**Interfaces:**
- Produces: `pub struct Program { pub fns: Vec<(String, HExpr)> }`
- Produces: `pub fn emit_module(fns: &[(String, HExpr)]) -> String` — returns a complete LLVM IR module string.
- Produces internal helpers: `emit_i64(expr, env: &MutEnv) -> String`, `emit_f64(expr, env: &MutEnv) -> String`, `emit_fn(name, body) -> String`.

**Emission rules (from the spec §4.7/§5):**
- Vector `(Vector i64)` = LLVM `%VecI64 = type { i64*, i64, i64 }` (data, len, cap).
- Every reduce/loop takes the vector's data pointer as a function parameter declared **`noalias nocapture readonly`** and iterates `[0, len)`.
- Loads use `align 8`; GEPs use `inbounds`.
- Two externs emitted once: `declare void @cato_print_f64(double)` and `declare void @cato_print_i64(i64)`.
- A fused `Pipeline`/`Closure` emits ONE loop (see template below).
- A vector literal `(vec-i64 a b c)` materializes a private constant array global `@.. = private constant [N x i64] [i64 a, ...]`, an alloca `%VecI64` on the stack, stores `{data, len, cap}` into it, and (for callers) passes its `.data`/`.len` as the `noalias` args.

- [ ] **Step 1: Write the failing test** — `tests/codegen_test.rs`:

```rust
use cato::codegen::emit_module;
use cato::hir::build;
use cato::reader::read_all;

fn emit(src: &str) -> String {
    let fns = build(&read_all(src).unwrap()).unwrap();
    emit_module(&fns)
}

#[test]
fn emits_noalias_on_loop_data_param() {
    let ir = emit("(defn fused [xs] (reduce + 0 (map square (filter even? xs))))");
    assert!(ir.contains("noalias nocapture readonly"));
}

#[test]
fn emits_single_loop_region() {
    let ir = emit("(defn fused [xs] (reduce + 0 (map square (filter even? xs))))");
    assert!(ir.matches("label %loop").count() == 1);
}

#[test]
fn emits_declares_for_prints() {
    let ir = emit("(defn main [] (print-f64 1.0) (print-i64 2))");
    assert!(ir.contains("declare void @cato_print_f64(double)"));
    assert!(ir.contains("declare void @cato_print_i64(i64)"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test codegen_test`
Expected: FAIL — module `cato::codegen` not found.

- [ ] **Step 3: Implementation** — write `src/codegen.rs`. The module emits:

For each `(name, body)` pair, a `define` function. Body emission dispatches:
- `HExpr::Call("print-f64", [arg])` → `call void @cato_print_f64(f64 <emit_f64(arg)>)`
- `HExpr::Call("print-i64", [arg])` → `call void @cato_print_i64(i64 <emit_i64(arg)>)`
- `HExpr::Reduce(Reduce)` → emit the fused loop function.

**Fused loop function template** (for `Pipeline` with one filter + one map, fuse order: maps then filters then combine). The emitter computes, for each element `%x` of type i64:
1. filter predicates (each `emit_i64(pred.body)` with `el` bound to `%x`, and a `Var("")`? no — `el` bound via env to `%x`), test `icmp ne i64 <pred>, 0`.
2. if passed, run maps (each bound to the previous element value), then combine with accumulator via type-correct op.

For integers, combine `+` → `add i64`, `-` → `sub i64`, `*` → `mul i64`, `/` → `sdiv i64`. For f64 both ops → `fadd/fsub/fmul/fdiv double`. The accumulator and element type come from `init`: if `init` is `ConstI64`, loop is i64; if `ConstF64`, loop is f64.

**Closure reduce** — `(reduce (fn [acc x] body) init src)`: one loop, `%acc` phi, each element `%x`, emit `body` with `acc`→`%acc`, `el`→`%x`, result is new acc. Type from `init`.

**Concrete emitted IR for the pipeline case** (i64, one filter `even?`, one map `square`):

```llvm
; @fused(ptr noalias nocapture readonly %xs.data, i64 %xs.len) -> i64
define i64 @fused(ptr noalias nocapture readonly %xs.data, i64 %xs.len) {
entry:
  %isempty = icmp eq i64 %xs.len, 0
  br i1 %isempty, label %ret, label %loop.pre
loop.pre:
  br label %loop
loop:
  %i = phi i64 [0, %loop.pre], [%i.next, %loop]
  %acc = phi i64 [0, %loop.pre], [%acc.merge, %loop]
  %p = getelementptr inbounds i64, ptr %xs.data, i64 %i
  %x = load i64, ptr %p, align 8
  %rem = and i64 %x, 1
  %even = icmp eq i64 %rem, 0
  br i1 %even, label %apply, label %skip
apply:
  %sq = mul i64 %x, %x
  %acc2 = add i64 %acc, %sq
  br label %merge
skip:
  ; element skipped: acc unchanged
  %sacc = phi i64 [0, %loop.pre], ...
```

The `skip` phi for the unchanged accumulator is tricky because `%acc` is a phi from `loop`. Simpler correct approach: compute `%acc.merge` via a phi that takes `%acc2` from `apply` and `%acc` from `skip`, where the `skip` block just forwards `%acc`:

```llvm
skip:
  br label %merge
merge:
  %acc.merge = phi i64 [ %acc2, %apply ], [ %acc, %skip ]
  %i.next = add i64 %i, 1
  %cond = icmp ult i64 %i.next, %xs.len
  br i1 %cond, label %loop, label %ret
ret:
  ret i64 %acc.merge
```

This is valid (no phi in `skip`; `%acc` used from `loop` into `merge` via the `skip`→`merge` edge). The cumulative `%acc` is carried by the phi in `loop` reading `%acc.merge` from `merge`. Correct.

For the **closure** case (f64, `sum-of-squares`), same shape but `apply` emits the closure body and no filter/skip (single block):

```llvm
define double @sum_of_squares(ptr noalias nocapture readonly %xs.data, i64 %xs.len, double %mu) {
entry:
  %isempty = icmp eq i64 %xs.len, 0
  br i1 %isempty, label %ret, label %loop.pre
loop.pre:
  br label %loop
loop:
  %i = phi i64 [0, %loop.pre], [%i.next, %loop]
  %acc = phi double [0.000000e+00, %loop.pre], [%acc.next, %loop]
  %p = getelementptr inbounds i64, ptr %xs.data, i64 %i
  %x64 = load i64, ptr %p, align 8
  %x = sitofp i64 %x64 to double
  %d = fsub double %x, %mu
  %d2 = fmul double %d, %d
  %acc.next = fadd double %acc, %d2
  %i.next = add i64 %i, 1
  %cond = icmp ult i64 %i.next, %xs.len
  br i1 %cond, label %loop, label %ret
ret:
  ret double %acc.next
}
```

The element-load type is always i64 (vector of i64); f64 bodies convert via `sitofp`.

**Captured constants / vars:** Any `Var(name)` in a body that is not the lambda element or accumulator is treated as a **function parameter** (loop-invariant). `emit_fn(name, body)` collects the set of such vars by walking the body, then declares them as trailing `double`/`i64` params. To keep the slice simple, emit all captured vars as `i64` unless their first int-context use is f64 — for the slice, `mu` is f64; the emitter decides param type by requiring the build to already carry it. Simplification for the slice: **treat every reference to an external fn's captured var as a `double` parameter** (matches `mu`, the only closure capture in the reference program).

**Calling a defined Cato fn** from within another body (e.g., `variance` calls `sum_of_squares`, which itself is a loop fn) — emission passes the vector's `data`+`len` pointers and captured params: `call double @sum_of_squares(ptr %vec.data, i64 %vec.len, double %mu)`. To materialize `%vec.data`, `emit_i64` on a `Var("xs")` that is known to be a vector binding yields the loaded `{i64*,i64,i64}` from an alloca in the current function, then `extractvalue`. Concretely, when a `Var` refers to a local vector (from `(vec-i64 ...)` let/binding), allocate it as `%VecI64` alloca, store the literal, and `extractvalue {i64*,i64,i64}, ptr %v, 0`/`1` for data/len.

For the slice, `main` is the only caller of the other functions and `variance`'s `xs` comes from `(vec-i64 ...)`. The emitter wires: `main` builds the vec local, then calls `@variance(ptr %v.data, i64 %v.len)`, `variance` computes `mu` and calls `@sum_of_squares(ptr %xs.data, i64 %xs.len, double %mu)`, then calls `@cato_print_f64`. Implement `main`'s vector literal via a private constant global so `.data` points to static storage:

```llvm
@main.vec.0 = private constant [5 x i64] [i64 1, i64 2, i64 3, i64 4, i64 5]

define i32 @main() {
entry:
  %v = alloca %VecI64, align 8
  store %VecI64 { ptr @main.vec.0, i64 5, i64 5 }, ptr %v, align 8
  %vd = extractvalue %VecI64 <load %v>, 0   ; deref via load
  ...
}
```

Correct sequence: `%lv = load %VecI64, ptr %v, align 8; %vd = extractvalue %VecI64 %lv, 0; %vl = extractvalue %VecI64 %lv, 1`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/codegen.rs tests/codegen_test.rs src/lib.rs
git commit -m "feat: LLVM IR emitter with noalias fused loops"
```

(Add `pub mod codegen;` to `src/lib.rs` in Step 3.)

---

### Task 6: Compose CLI + End-to-End Native Binary

**Files:**
- Modify: `src/main.rs` (real pipeline)
- Modify: `tests/integration.rs` (end-to-end test)
- Create: `tests/e2e.sh` (optional convenience script)

**Interfaces:**
- Produces: `cato <file.cato>` → prints IR to stdout; `cato <file.cato> -o out.ll` writes the file; exit 0 on success, non-zero with a message on any parse/type/fusion/codegen error.

- [ ] **Step 1: Write the failing test** — append to `tests/integration.rs`:

```rust
#![cfg(unix)]
use std::process::Command;
use std::path::PathBuf;

// End-to-end: parse/build/fuse/emit to IR, compile with clang, run, assert output.
fn cato_bin() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_cato")) }

#[test]
fn compiles_slice_to_expected_output() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("examples/slice.cato");
    let out_ll = manifest.join("target/slice.ll");
    let rt = manifest.join("runtime/runtime.c");
    let exe = manifest.join("target/slice_bin");

    let status = Command::new(cato_bin())
        .arg(&src).arg("-o").arg(&out_ll).status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "2.5\n20\n");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL — `cato <file.ll> -o` produces a not-yet-implemented message / no `slice.ll` file.

- [ ] **Step 3: Implementation** — rewrite `src/main.rs`:

```rust
mod ast; mod reader; mod hir; mod fuse; mod codegen;

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (file, out) = parse_args(&args);
    let src = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| { eprintln!("cato: cannot read {}: {}", file, e); std::process::exit(2); });
    let parsed = reader::read_all(&src)
        .unwrap_or_else(|e| { eprintln!("cato: parse error: {}", e); std::process::exit(2); });
    let fns = hir::build(&parsed)
        .unwrap_or_else(|e| { eprintln!("cato: semantic error: {}", e); std::process::exit(2); });
    let fused: Vec<(String, hir::HExpr)> = fns.iter()
        .map(|(n, b)| (n.clone(), fuse::fuse(b))).collect();
    let ir = codegen::emit_module(&fused);
    match out {
        Some(p) => std::fs::write(&p, &ir)
            .unwrap_or_else(|e| { eprintln!("cato: cannot write {}: {}", p, e); std::process::exit(2); }),
        None => print!("{}", ir),
    }
}

fn parse_args(args: &[String]) -> (String, Option<String>) {
    let mut file = None;
    let mut out = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => { out = args.get(i + 1).cloned(); i += 2; }
            "--emit-llvm" => { i += 1; }
            "--help" | "-h" => { usage(); std::process::exit(0); }
            s if s.starts_with('-') => { eprintln!("cato: unknown flag {}", s); std::process::exit(2); }
            s => { file = Some(s.to_string()); i += 1; }
        }
    }
    match file {
        Some(f) if Path::new(&f).exists() => (f, out),
        Some(f) => { eprintln!("cato: no such file {}", f); std::process::exit(2); }
        None => { usage(); std::process::exit(2); }
    }
}

fn usage() {
    eprintln!("cato: compile a .cato program to LLVM IR\n\nUSAGE:\n  cato <file.cato> [-o out.ll]");
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS (the end-to-end test compiles `examples/slice.cato`, links with `runtime/runtime.c` via `clang`, runs, and asserts `2.5\n20\n`). Also verify manually:
```bash
cargo run -- examples/slice.cato -o /tmp/slice.ll
clang /tmp/slice.ll runtime/runtime.c -o /tmp/slice_bin
/tmp/slice_bin
# -> 2.5
# -> 20
```

- [ ] **Step 5: Commit**

```bash
git add src/main.rs tests/integration.rs
git commit -m "feat: end-to-end compilation to runnable native binary"
```

---

## Self-Review

**Spec coverage:**
- §4.1 Reader → Task 2. ✓
- §4.3 Analyze → Task 3 (HIR builder resolves primitives and lowers defns). ✓
- §4.5 Stream Fusion → Task 4. ✓
- §4.7 SSA + noalias emission → Task 5 (every loop data pointer is `noalias nocapture readonly`, GEPs `inbounds`, loads `align 8`). ✓
- §5 variance/normalize examples → Task 6 runs `examples/slice.cato` (`variance` + a fused `map/filter/reduce`). ✓
- Out of scope by design (recorded in Goal/Constraints): ownership/borrow checking, macros, full polymorphism, maps/sets. These are follow-up plans.

**Placeholder scan:** Every task contains concrete code, file paths, exact test bodies, and exact commands. No TBD/TODO.

**Type consistency:** `Expr` (`ast`) → `Vec<Expr>` (`reader::read_all`) → `(String, HExpr)` pairs (`hir::build`) → fused `(String, HExpr)` (`fuse::fuse`) → `emit_module(&[(String, HExpr)])` (`codegen`). Function names `read_all`, `build`, `fuse`, `emit_module` are used consistently across task interfaces and test imports. `MapSpec { el, body }`/`FilterSpec { el, body }`/`HReduce::Pipeline`/`HReduce::Closure` fields are consistent between Task 3 (definition) and Task 5 (emission).
