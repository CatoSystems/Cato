# S1 — Generic & recursive ADTs with deterministic destruction

Date: 2026-09-15
Goal: the language floor for the self-hosted kernel — algebraic data types
with type parameters, recursion with selective boxing, and a no-GC memory
model that bounds memory in long-running compiler workloads.

Guiding principle: **S1 establishes the language's memory and representation
semantics; compiler-specific performance techniques remain implementation
details.** Nothing in this spec adds a language feature because the compiler
implementation would benefit from it.

## The three layers

```text
LANGUAGE CONTRACT        what any Cato program may rely on
- ADT semantics (closed world, monomorphized)
- representation rule (inline by default, boxed only where recursion requires)
- ownership/lifetime semantics
- deterministic destruction (scope-exit drops, transitive)

RUNTIME                  what the emitted program links against
- malloc/free
- drop glue (compiler-emitted free sequences)
- ABI (aggregate layout, boxed pointer convention)
- termination (frees only; no unwinding, no user destructors in S1)

COMPILER IMPLEMENTATION  kernel architecture, never language
- AST arenas + NodeId abstraction
- symbol interning (SymbolId) at the reader
- integer-keyed tables, string-builder emission
- free lists, pools
```

## 1. ADT semantics (language contract)

- `(deftype (Option a) (Some [v :- a]) None)` — type parameters on deftype;
  fields may name the deftype's own parameters. Closed world: all types are
  known at compile time; there is no dynamic dispatch over types.
- Like generic `defn`s, generic ADTs **monomorphize**: every use resolves to a
  concrete instance (mangled `Option__i64`-style, deduped). The
  `unify`/`subst_ty`/`instantiate` machinery extends from functions to types.
- Non-generic ADTs are unchanged: `(deftype Shape ...)` keeps today's
  declaration, checking, and codegen.

### Representation rule (the load-bearing decision)

- An ADT is **inline/unboxed** whenever its transitive field types are of
  bounded, known size — today's `%ADT_N = { i64 tag, [N x i64] }` stack-slot
  layout, unchanged. `Option i64`, `Result i64 str`, scalar enums: no heap
  allocation, ever.
- An ADT is **boxed** when indirection is required, from either of two
  causes: it is **recursive** (on a cycle of ADT-typed fields), or it
  **owns heap** (has string/vector fields, transitively) — ownership needs
  a unique address, so owning values move by pointer exactly like boxed
  ones. A boxed ADT's value is a pointer to a heap `{ i64 tag, fields... }`;
  the constructor allocates; `match` and field access dereference.
- Boxing is computed per concrete (monomorphized) ADT — never applied
  conservatively to a whole family.
- No arena types, no `ref`/`box` expressions, no allocation primitives enter
  the language. Recursion is expressed in ordinary ADT syntax and the
  compiler picks the representation.

## 2. Ownership & deterministic destruction (language contract)

Cato has no GC and no runtime collector; **destruction is deterministic and
lexical**:

- Ownership transfers by move (binding, assignment, argument pass). The new
  owner is responsible for destruction.
- A binding that owns heap memory (a boxed ADT, a vector, a map, a set, a
  heap string) is **dropped at scope exit** unless it was moved, returned, or
  escaped — the move checker already tracks exactly these states.
- Drops are **transitive**: dropping an ADT drops fields that own heap
  (recursively through nested ADTs). Drop order: fields in reverse
  declaration order, then the box.
- Shared (`&`) and exclusive (`&!`) borrows never free. Returning transfers
  the value and its destruction duty to the caller.
- **Strings join the same model** — `subs`/`split`/`join`/`str`/`char->str`
  results are owned heap values that drop at scope exit; string literals
  remain static data (not owned, never freed). No permanent string special
  case survives S1.
- Dropping is unobservable: no user-defined destructors, no `Drop` trait, no
  side effects at scope exit in S1. (A `drop` statement exists for early,
  explicit release; using a moved value stays a compile error.)
- **Free-list reuse is an implementation optimization, not contract**: the
  spec guarantees memory is released deterministically; it does not promise
  which allocator call performs it. The existing free-list transfer in the
  optimizer implements the guarantee cheaply.
- Cycles are not collected (nothing is): a recursive structure that owns
  itself leaks, like Rust's `Rc` cycle. Reference structures must route
  through non-owning indirection (ids).

## 3. Compiler implementation conventions (not language)

These bind the self-hosted kernel's architecture; they deliberately have no
syntax:

- **Arenas + `NodeId`.** The kernel's AST/HIR/token storage lives in vector
  pools; nodes reference each other through a `NodeId` abstraction (initially
  `i64` indexes) behind a small module boundary, so the representation can
  change without touching pass code. The kernel does *not* build its AST from
  boxed recursive ADTs.
- **Interning at the reader.** The reader maps each distinct symbol to a
  `SymbolId` (i64) exactly once; every downstream phase keys tables on
  integers, never re-hashing or comparing strings on hot paths. The intern
  table itself is built once per compilation.
- **Emission** accumulates into a `(Vector str)` builder joined once.
- **No mutation.** Passes stay functional/persistent — the seed's
  clone-heavy rewrites demonstrate sufficient headroom. If self-hosting
  measurements show a concrete copying/rewrite bottleneck, controlled
  mutation is revisited then, not before.
- These conventions land with the kernel slices (S4+); S1 only reserves the
  boundary (no language feature may depend on them, and they may not leak
  into `.cato` source).

## 4. Sub-slices

**S1a — Generic, non-recursive ADTs (this slice).**
Let-binding type ascription (`[x :- (Option i64) (None)]`) lands with this
slice: monomorphized nullary ctors (`(None)`) carry no value to infer their
parameter from, so a binding can pin the expected type — the same role as
Rust's `let x: Option<i64> = None;`. Full unification-based inference stays
out of scope.
`deftype` type parameters; annotations `(Option i64)` resolve eagerly at
parse time to concrete instances (no new `Ty` variant — `Ty` is matched in
~50 places, and monomorphized instances are ordinary concrete ADTs);
constructor calls unify argument types against template fields and
instantiate; `match` resolves variants against the instance table;
monomorphization of generic `defn`s re-instantiates ADT applications in
their signatures (`ty_suffix` learns ADT names for mangling). Backend work:
none for layout (instances are today's inline ADTs) plus the missing `i32`
field arm in `adt.rs`. Fields in S1a: `i64 i32 f64 bool` + type parameters.

**S1b — Recursion, selective boxing, full field types** *(shipped)*: field grammar widened (str, (Vector t), applied-template references with the positional-parameter rule); recursive instantiation terminates through the instance cache (registered before fields resolve); boxing = exactly the ADTs on a cycle of ADT-field edges (`compute_boxed`), discovered by DFS — everything else, including ADTs that merely *contain* a boxed one, stays inline; boxed values are owning heap pointers that pass/return by pointer and deep-clone through a compiler-generated per-type helper; match-arm bodies and ctor arguments infer from the expected instance's concrete fields; a defn's declared return type threads into body lowering as the expected type. Reference: `examples/trees.cato` (boxed, recursive) next to `examples/options.cato` (unboxed, S1a). Interim memory contract for boxed ADTs: boxes live until process exit until S1c lands the frees.
Recursion detection over the instantiated ADT graph; boxed representation
for exactly the recursive set; fields widen to any type (`str`, `(Vector t)`,
ADT — which is what *makes* recursion expressible); ownership treats boxed
ADTs and heap-holding fields as moving/owning; ctors allocate, `match`
dereferences.

**S1c — Drop glue & strings** *(shipped)*: `compute_adt_facts` derives
boxing and ownership jointly (boxed = cycle ∨ owning). Free annotation is
value-shape based: only *fresh* allocations free at scope exit (subs/split/
join/str/char->str, collections, boxed ctors, fresh returns) — aliases,
element loads, and literals (static) never free; anything uncertain leaks,
nothing double-frees. Owned move-type parameters free at body exit unless
tail-escaped. ADT string fields are `strdup`'d at construction and deep-
cloned; per-owning-ADT drop glue frees transitively. One optimizer rule was
tightened: clone elision now requires a transferable free-list entry (or a
static source) — eliding a clone of a drop-glue-owned child previously
shared a dying buffer. Validated: `trees`/`options`/`strings2`/`own`/
`heap`/`hamt`/`strings` all ASAN-clean with unchanged output, opt on and
off.
Transitive owns-heap analysis; scope-exit drop emission for boxed ADTs and
heap-holding fields; heap strings join the owned world; ASAN-verified
(reference: a program that builds and discards trees in a loop with flat
RSS); free-list reuse tuned where profiles say so.

## 5. Validation

- Each sub-slice: spec'd invariants TDD'd, reference program under
  `examples/`, byte-identical interp↔LLVM parity, `--asan` clean on the
  owning paths.
- From S4: CI times the seed compiling `selfhost/` (kernel codegen cost
  always visible). From S8: the bootstrap job records **wall-clock and peak
  RSS of cato1 self-compiling** next to the fixpoint diff. The seed-vs-cato1
  ~5x ratio is a diagnostic threshold for finding representation bugs, not a
  permanent guarantee: if exceeded, investigate allocation shape, copying,
  string handling, symbol lookup, and reclamation — in that order — before
  ever considering mutation or new language machinery.

## 6. Non-goals

`set!`/interior mutability; arenas or allocation primitives in the language;
user-defined destructors; cyclic collection; arena types behind `deftype`;
runtime type information; reflection.
