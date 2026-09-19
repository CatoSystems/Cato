# Cato: A High-Performance Systems Functional Language

**Design Specification**
Date: 2026-09-07
Status: Draft for review

---

## 1. Language Overview & Vision

**Cato** is a statically-typed, compiled systems programming language with Clojure's surface syntax. It retains S-expressions, a Lisp-style macro layer (a fixed macro set today; user-defined macros are scheduled after the self-hosting fixpoint), and the immutable-first functional philosophy, but compiles to native machine code via LLVM with Rust-like ownership semantics.

### Core Philosophy

| Principle | How Cato Delivers It |
|---|---|
| **Clojure ergonomics** | S-expression syntax, `[]` vectors, `{}` maps, `#{}` sets, threading macros (`->`, `->>`), Lisp macros |
| **Systems-level performance** | Direct LLVM IR emission, monomorphization, zero-cost abstractions, no GC |
| **Memory safety without runtime cost** | Compile-time ownership/borrowing with linear types — enforced at compile time, zero runtime overhead |
| **Zero-allocation functional code** | Stream fusion, closure conversion, and inlining eliminate heap allocations from `map`/`filter`/`reduce` pipelines |

### What Cato Is NOT

- **Not Clojure-compiled-to-native** — it has a proper static type system, not dynamic dispatch with type hints.
- **Not Rust with Lisp syntax** — the ownership model is Rust-inspired, but the macro system, data literals, and functional idioms are Clojure-native.
- **Not a research language** — designed for production systems programming: FFI to C, OS interfaces, embedded targets.

### Design Trade-offs

| Trade-off | Decision | Rationale |
|---|---|---|
| Generality vs. Safety | Ownership enforced, not optional | Systems programming demands memory safety guarantees |
| Syntax simplicity vs. Expressiveness | Retain Lisp macro power, in stages | A fixed macro set ships with the seed compiler; user-defined macros (`quote`/`syntax-quote`) are scheduled after the self-hosting fixpoint |
| Compile time vs. Run time | Aggressive monomorphization | Accepts longer compile time for zero-allocation runtime and SIMD opportunity |
| Interop vs. Purity | C FFI is first-class | Systems programming requires talking to C libraries |

---

## 2. Type System & Memory Management

### Type System: Hindley-Milner with Bidirectional Inference + Ownership

Cato uses a **bidirectional checker**: an expected type flows down through lowering, and a structural unifier relates annotations to argument types. Inference is **monomorphic** — generics are defn-level type parameters, monomorphized per call site (no let-polymorphism; every binding has one concrete type), extended with linear/affine types for ownership tracking.

**Core Properties:**
- **Inferred by default** — no type annotations required for local definitions.
- **Opt-in annotations** — `(defn add [x :- i64 y :- i64] :- i64 (+ x y))`.
- **Monomorphic inference** — the compiler infers concrete types, not type variables (no boxing, no dynamic dispatch).
- **Algebraic data types** — `(deftype ...)` with exhaustive pattern matching via `match`.

### Type Representation in S-expressions

```clojure
;; Fully inferred — compiler deduces (i64, i64) -> i64
(defn add [x y]
  (+ x y))

;; Opt-in annotations — useful for public APIs or complex functions
(defn add [x :- i64 y :- i64] :- i64
  (+ x y))

;; Generic functions — inferred, monomorphized at call sites
(defn first [coll]
  (get coll 0))

;; Algebraic data type
(deftype Option
  (Some [value])
  (None []))

;; Pattern matching
(defn unwrap-or [opt default]
  (match opt
    (Some v) v
    (None)   default))
```

### Ownership & Borrowing Model

Inspired by Rust's borrow checker, expressed in Clojure's threading/let-binding style:

```clojure
;; Ownership transfer — `buf` is moved into `process`; cannot be used after
(let [buf (alloc-buffer 1024)
      result (process buf)]   ;; buf is MOVED here
  (use result))               ;; buf is gone — compiler enforces this

;; Immutable borrow — `&` marks a shared reference, no mutation allowed
(let [data (load-config)
      summary (summarize &data)]   ;; borrows data immutably
  (print summary)
  (print data))                    ;; data still valid — only borrowed

;; Mutable borrow — `&!` marks exclusive access
(let [counter (new-counter 0)
      _ (increment! &!counter)]   ;; exclusive mutable borrow
  (get counter))                   ;; borrow ends when &! goes out of scope
```

**Ownership Rules:**

| Syntax | Meaning | Equivalent Rust |
|---|---|---|
| `(let [x (expr)])` | `x` owns the result; moved on next use | `let x = expr;` |
| `&x` | Immutable shared borrow | `&x` |
| `&!x` | Mutable exclusive borrow | `&mut x` |
| `(clone x)` | Explicit deep copy | `x.clone()` |
| `(drop x)` | Explicit deallocation | `drop(x)` |

> **Implementation status.** What the checker enforces today: moves and
> use-after-move, and borrowed (`&`) parameters cannot be moved anywhere in
> the callee (passing one to an owned parameter, capturing, `conj`-ing, or
> dropping it is a compile error — the callee could free a buffer the caller
> still reads). `&!` exclusive borrows were removed pre-1.0: with
> uniqueness-driven in-place updates there is no user-facing exclusive lend.
> Destruction is ownership-driven (`plan_drops`): a binding frees at scope
> exit iff it still owns its heap buffer; moves transfer the duty. Borrow
> *regions* (borrows beyond a call boundary, mutation through borrows) are
> not part of the language; `&` is a checked parameter-passing mode.

### Unboxed Native Types

All types are unboxed by default — no boxing, no vtables, no type tags at runtime:

| Cato Type | LLVM Representation | Size |
|---|---|---|
| `i8`, `i16`, `i32`, `i64` | Integer | 1/2/4/8 bytes |
| `f32`, `f64` | Float | 4/8 bytes |
| `bool` | i1 | 1 bit |
| `(Array T)` | `{ptr, len, cap}` | 24 bytes (64-bit) |
| `(Map K V)` | HAMT tree node | Contiguous arena |
| `(String)` | `{ptr, len}` | 16 bytes |
| `(fn [x] ...)` | Function pointer + captured env (if any) | 8 or 16 bytes |
| `(deftype Foo ...)` | Tagged union or unboxed struct | Compile-time decided |

### Stack Allocation Heuristic

The compiler aggressively stack-allocates:
- Values smaller than 256 bytes with known lifetime.
- Intermediate collections in pipeline expressions (`->`, `->>`).
- Closure environments that don't escape.

Heap allocation is **explicit** via `(heap expr)`, or happens only when the compiler cannot prove the value's lifetime is bounded.

---

## 3. Core Data Structures Implementation

### Vectors — Cache-Conscious Chunked Arrays

Cato vectors are **contiguous chunked arrays** (RRB-tree-flattened), not pointer-heavy heap trees like Clojure JVM.

**Layout:**
```
Vector (i64) = { Chunk* data, usize len, usize cap }
Chunk = contiguous i64[CAP]   // CAP = 64 or 256 elements
```

**Structural sharing with append:**
```
[1 2 3 4]  ->  data: [1 2 3 4 . . . .]  len=4 cap=8
(conj v 5) ->  data: [1 2 3 4 5 . . .]  len=5 cap=8  (share array, bump len)
```

- Appends are O(1) amortized with no allocation (write to next slot + bump len).
- Indexing is a direct pointer dereference + offset — one cache line.
- `subvec` shares the underlying buffer with offset/length — zero copy.
- Buffer overflow allocates a new chunk and links (RRB-tree), O(1) amortized.
- SIMD-friendly: elements are contiguous, so LLVM auto-vectorizes folds.

### Maps — Cache-Conscious Hash-Array Mapped Trie (HAMT)

Cato maps use a **flattened HAMT**: a path-compressed hash trie where each node is a contiguous array of entries, not a sparse node with pointers.

```
Node = { u32 bitmap, Entry entries[] }   // entries stored contiguously
Entry = { KeyHash u64, Key k, Value v }
```

**Why this beats pointer-heavy trees:**
- Each trie node's entries are contiguous → one cache line covers several lookups.
- The bitmap indicates which slots in the dense `entries` array are populated.
- Lookups traverse at most 6-7 nodes for 32-bit keys (one cache line each).

**FlatMap for small maps:** Maps with ≤ 8 entries compile to a **flat contiguous struct** `{ u64 hash[8], K keys[8], V vals[8], u8 n }` — one allocation, one cache line, linear/vectorized scan. This is the common case (config maps, struct-like data).

### Sets — Specialized HAMT

Identical to maps but with keys only. Sets of integers use a **bit-packed** representation for dense ranges.

### Sequences — Lazy But Fused at Compile Time

Cato's `seq`, `map`, `filter`, `reduce` are **not runtime lazy** — they're *compile-time* lazy. The sequence abstraction exists as a type constructor, but at compile time (via stream fusion, Section 4):

```
(reduce + (map square (filter even xs)))
```
becomes:
```
for (i = 0; i < xs.len; i++) {
    if (is_even(xs[i])) acc += square(xs[i]);
}
```

**Zero allocations. Zero iteration objects. One fused loop.**

Sequences are only materialized as runtime objects when:
- The consumer and producer types can't be fused (rare).
- Sequences escape the function (returned from a function).

### Strings — Copy-on-Write Byte Buffers

`(String)` is `{ u8* ptr, usize len, usize cap }` — contiguous UTF-8, no rope structures. Shared substrings use `{ptr+offset, len}` views internally; writes trigger a copy.

### Why Cato Beats Pointer-Heavy C

| Operation | Clojure JVM (PersistentVector) | Cato Chunked Vector | Speedup |
|---|---|---|---|
| Lookup | Navigate 6+ pointer hops, cache misses | Single dereference + offset | ~10-20x |
| Append | Allocate new leaf, pointer hops | Bump len, write slot | ~5x + no GC pressure |
| Iterate | Iterator allocation, boxed elements | Direct pointer walk, unboxed | ~10x + SIMD possible |
| Map lookup | ~log32(n) pointer-hop nodes | 1 cache line per node, contiguous | ~5-10x |

---

## 4. Compilation Pipeline

### Pipeline Overview

```
S-expression source
    │
    ▼
[1] READER  ────────────────►  S-expression AST (homogeneous data)
    │
    ▼
[2] MACRO EXPANSION ────────►  Expanded forms (syntax quote, threading macros)
    │
    ▼
[3] ANALYZE + OWNERSHIP ────►  Typed AST + borrow-checked (ownership graph)
    │
    ▼
[4] MONOMORPHIZATION ───────►  Fully concrete, monomorphic IR (no type variables)
    │
    ▼
[5] STREAM FUSION ──────────►  Fused data-flow graph (no intermediate allocations)
    │
    ▼
[6] DEFUNCTIONALIZATION ────►  Higher-order functions → first-order code (closure conversion)
    │
    ▼
[7] SSA / OPTIMIZATION ─────►  LLVM SSA form + canonicalization
    │
    ▼
[8] LLVM IR EMISSION ───────►  `.ll` / `.bc` → native object via LLVM backend
```

### Stage-by-Stage Detail

#### [1] Reader
Parses S-expressions into a homogeneous AST. No type checking here; pure syntax.

#### [2] Macro Expansion
Full Clojure-style macro system. Macros operate on the untyped AST. `->`, `->>`, `when`, `if-let`, `for`, `doseq` all expand here. Standard-library macros are precompiled; user macros are compiled to Cato and run in the compiler host process.

```clojure
(-> xs (filter even?) (map square) (reduce + 0))

;; Expands to:
(reduce (fn [acc x] (+ acc x)) 0
  (map (fn [x] (* x x))
    (filter (fn [x] (even? x))
      xs)))
```

#### [3] Analyze + Ownership Checking
The **bidirectional type checker** runs here and simultaneously produces the ownership/borrow graph:
- Every expression gets a type via bottom-up inference (checking) and top-down expectations (inference).
- Ownership: every let-binding is assigned a *lifetime* and a *movability* — linear (consumed once), affine (consumed at most once), or shared.
- The borrow checker verifies: no use-after-move, no concurrent `&!` borrows, borrows end before owner moves.

#### [4] Monomorphization
Every polymorphic function is instantiated per concrete argument type. This:
- Eliminates boxing (no type erasure, no vtables).
- Enables per-type code optimization (SIMD for `f64`, bit tricks for `u32`).
- Increases compile time but makes the runtime free of dispatch.

Monomorphization is interprocedural — the whole program's call graph is instantiated before codegen.

#### [5] Stream Fusion
Rewrites data-flow pipelines recognized by library functions (`map`, `filter`, `reduce`, `take`, `drop`, `zip`, `range`, `take-while`) into a single loop nest. Eliminates intermediate vector allocations, iteration-object allocations, per-element function-call overhead, and enables LLVM to vectorize the resulting simple loop.

```
Input IR:
  reduce(+) 0 (map square (filter even xs))

Fused IR:
  acc = 0
  for i in 0..xs.len:
      x = xs[i]
      if even(x):
          acc += square(x)
  return acc
```

#### [6] Closure Conversion (not defunctionalization)
Any higher-order function that wasn't fused (e.g., a callback passed to a C library) is **closure-converted**: each closure becomes `{ fn* pointer, void* env }` with an indirect call. Since monomorphization made every closure type concrete, there is no type erasure and no dynamic dispatch; after inlining, most calls become direct calls. (True defunctionalization — enumerating every call target into a static dispatch structure — is a possible later optimization, not what ships.)

#### [7] SSA + Noalias Emission
Convert to LLVM SSA. The target state is **`noalias`** on all function arguments and load/store pointers: because the language enforces immutability + ownership, the compiler can in principle prove:
- No two mutable references alias.
- Immutable references never alias mutable ones.
- Fused loops write to disjoint memory regions.

**Shipped today:** `noalias nocapture readonly` on fused-loop data and closure-env *parameters only*; loads/stores carry no alias metadata yet. Emitting noalias per load/store requires the uniqueness proof at each access point rather than per binding — this is the planned work, and until it lands the auto-SIMD/auto-parallel claims below rest on LLVM's default alias analysis over the parameter annotations.

#### [8] Automatic Parallelism
After fusion + noalias analysis, the compiler identifies pure loops (no side effects, no cross-iteration data dependence). Candidates for:
- **Auto-vectorization** — LLVM's loop vectorizer (guaranteed by noalias + pure function bodies).
- **Auto-parallelization** — parallel reductions using `llvm.loop.parallel`, or split independent iterations across threads — no locks needed because fusion guarantees disjoint memory.

Opt-in per-loop via `(parallel loop)`; globally via a `--fast` flag.

**Shipped today:** `(parallel (reduce ...))` lowers pipeline reduces onto a thread pool, and `--fast` attaches `llvm.loop.vectorize.enable` metadata. The "fusion guarantees disjoint memory" argument is asserted, not yet backed by a proof pass; broader loop shapes are rejected rather than parallelized.

#### [9] LLVM IR Emission
Emit textual LLVM IR (`.ll`) with:
- Unboxed native types.
- `noalias nocapture readonly` on fused-loop data and closure-env parameters (function-level attributes and load/store alias annotations pending, §[7]).
- `inbounds` on all GEPs.
- Alignment hints derived from the data layout.

Then clang/LLVM handles instruction selection, register allocation, and machine codegen.

---

## 5. Code Examples

### Numerical Array Processing — Variance

```clojure
;; File: stats.cato
;; Compute the sample variance of a vector of i64 values.
;; Clojure syntax, compiled to zero-allocation native code.

(defn mean [xs :- (Vector i64)] :- f64
  (/ (reduce + 0 xs) (f64 (count xs))))

(defn sum-of-squares [xs mu]
  (reduce (fn [acc x]
            (let [d (- (f64 x) mu)]
              (+ acc (* d d))))
          0.0
          xs))

(defn variance [xs] :- f64
  (let [n  (count xs)
        mu (mean xs)
        ss (sum-of-squares xs mu)]
    (/ ss (f64 (- n 1)))))
```

### Pipeline-Heavy Example (Showcases Stream Fusion)

```clojure
(defn normalize [xsmean xsrng]
  ;; Subtract mean, scale by range, clamp to [-1, 1].
  ;; Threads through map/filter/reduce but fuses into ONE loop.
  (->> xs
       (map (fn [x] (/ (- x xsrng) xsrng)))
       (map (fn [x] (clamp x -1.0 1.0)))
       (reduce (fn [acc x] (max acc (abs x))) 0.0)))
```

### Generated C Equivalent (Semantics)

```c
// Generated semantics of (variance xs) — after fusion + defunctorialization
typedef struct {          // unboxed (Vector i64) representation
    int64_t *data;
    size_t   len;
    size_t   cap;
} Vec_i64;

double variance(Vec_i64 xs)   // noalias implicit
{
    // fused pass 1: mean (reduce +), single loop, no temp vector
    __int128 acc_sum = 0;              // wide accumulator, no overflow
    for (size_t i = 0; i < xs.len; i++)
        acc_sum += xs.data[i];

    const double n   = (double)xs.len;
    const double mu  = (double)acc_sum / n;

    // fused pass 2: sum-of-squares (reduce over map), single fused loop
    double ss = 0.0;
    for (size_t i = 0; i < xs.len; i++) {
        double x  = (double)xs.data[i];     // unboxed, contiguous
        double d  = x - mu;
        ss += d * d;                         // pure + noalias -> SIMD
    }

    return ss / (n - 1.0);
}
```

### Generated High-Level LLVM IR

```llvm
; ModuleID = 'stats.fused.ll'
target triple = "x86_64-unknown-linux-gnu"

define double @sum_of_squares(ptr noalias nocapture readonly %xs.data,
                              i64 %xs.len) {
entry:
  %is_empty = icmp eq i64 %xs.len, 0
  br i1 %is_empty, label %done, label %loop.preheader

loop.preheader:
  br label %loop

loop:                                            ; preds = %loop, %loop.preheader
  %i     = phi i64 [ 0, %loop.preheader ], [ %i.next, %loop ]
  %acc   = phi double [ 0.000000e+00, %loop.preheader ], [ %acc.next, %loop ]

  %ptr   = getelementptr inbounds i64, ptr %xs.data, i64 %i
  %x     = load i64, ptr %ptr, align 8

  %x64     = sitofp i64 %x to double
  %d       = fsub double %x64, %mu
  %d2      = fmul double %d, %d
  %acc.next = fadd double %acc, %d2

  %i.next  = add i64 %i, 1
  %cond    = icmp ult i64 %i.next, %xs.len
  br i1 %cond, label %loop, label %loop.exit

loop.exit:                                        ; preds = %loop
  br label %done

done:                                             ; preds = %loop.exit, %entry
  %result = phi double [ 0.000000e+00, %entry ], [ %acc, %loop.exit ]
  ret double %result
}
```

Key annotations:
- **`noalias`** on `%xs.data` — unlocks SIMD vectorization (`<4 x double>`).
- **`nocapture readonly`** — allows `load` hoisting and register reuse.
- **`inbounds`** on GEP — disables bounds overflow checks.
- **`align 8`** — enables aligned vector moves (`vmovapd` not `vmovupd`).

### Auto-Vectorized Result (what LLVM actually emits)

```llvm
loop.vector.body:
  %wide.load = load <4 x i64>, ptr %ptr, align 8
  %x420      = sitofp <4 x i64> %wide.load to <4 x double>
  %d4        = fsub <4 x double> %x420, splat(double %mu)
  %d24       = fmul <4 x double> %d4, %d4
  %acc4      = fadd <4 x double> %acc4.vec, %d24
  ...reduce to scalar at the end
```

**Result: 4 (or 8 with AVX-512) elements per SIMD instruction, zero allocations, cache-friendly contiguous traversal.**

### FFI to C (Systems-Programming Showcase)

```clojure
;; Declare a foreign C function
(ffi extern "write"
  [fd :- i32 buf :- (Ptr (Array i8)) count :- usize] :- isize)

;; Use it — direct call, no marshaling, no runtime
(defn write-all [fd data]
  (let [written (write fd data (count data))]
    (assert (= written (isize (count data))) "short write!")))
```

---

## Summary of Design Decisions

- **Cato** — Clojure-syntax systems language compiling to native via LLVM.
- **Ownership + Borrowing** (`&`/`&!`) for memory safety with zero runtime cost.
- **Bidirectional Hindley-Milner inference** — looks untyped, compiles strictly typed, unboxed.
- **Chunked vectors, flattened HAMTs, copy-on-write strings** — beat pointer-heavy cache misses.
- **Pipeline**: Reader → Macro Expansion → Analyze/Ownership → Monomorphize → Stream Fusion → Defunctionalize → SSA/noalias → LLVM IR.
- **`noalias` + purity + fusion** unlock auto-SIMD and auto-parallelization.
