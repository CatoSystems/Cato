# Cato Memory Model v3 — Audit, Competing Designs, Target, Plan

Date: 2026-10-19
Status: **Accepted for staged implementation.** Supersedes nothing wholesale:
it completes `2026-09-18-cato-ownership-model-revised.md` (v2), whose rules
are kept, and adds the parts v2 left out (loops, FFI ownership, pointers,
parameter modes, allocation strategy). Every repository claim below is
marked **[fact]** (cited, or reproduced by a command in §1.9) or
**[hypothesis]** (not yet measured).

Measurement tools added by this work: `tools/alloc_live.c` (live heap:
peak, at-exit, churn), `tools/alloc_site.c` (live bytes by allocating
function and requester; with `ALLOC_SITE_ROOTS=1`, *root leaks* ranked by
retained bytes — where ownership was lost, not where leaves were
allocated), `tools/mem_bench.sh` (seed / k1 / k2 compiler baseline),
`tools/leak_site.sh` (attribute a probe program's leaks).

---

## Part I — Audit

### 1.1 The system as built **[fact]**

```
source → reader (reader.rs | reader/read.cato) → expand (9 fixed macros)
       → modules → lower to typed HIR (monomorphizing, hir/lower.rs |
         hir/lower.cato) → ownership planner (hir/ownership.rs |
         hir/moveck.cato): move check + drop placement + in-place verdicts
       → opt (seed only: fold, DCE, inline, clone elision) → re-plan
       → LLVM text (cato-backend-llvm | emit/*.cato) → clang + runtime.c
```

Value representations: scalars/bool unboxed; `str` = NUL-terminated
`char*` (no length: `count` is `strlen`, O(n)); vectors = `{ptr,len,cap}`
header with elements `i64 | f64 | str` only; maps/sets = `i64` only (flat
sorted arrays, HAMT with per-node refcounts for large maps); ADTs inline
unless on a recursion cycle or owning (then one `malloc`'d box per value,
`{tag, fields…}`); closures `{code, env, drop}`. There is **no pointer
type** (`hir/mod.rs:8-42`). FFI is `extern` over scalars, `str` and
vectors.

Memory semantics today (v2): scalars and `str` are Copy; everything else
is affine. Passing to a parameter moves unless the callee declares it `&`
(call-scoped lend). Collections are persistent; `conj` mutates in place
only when the planner proves the source dead (`in_place`). Drops are
planner-placed, per-type monomorphized glue (`@drop__T`), null-tolerant.

### 1.2 Baseline measurements **[fact]** (`tools/mem_bench.sh`)

Compiling `selfhost/cato/cli.cato` (858 KB of Cato, 28 k lines) to IR
(`--emit-ir`, compiler process only — clang excluded):

| compiler | wall | max RSS | peak live | live at exit | churn | allocs | frees |
|---|---|---|---|---|---|---|---|
| seed (Rust) | 0.58 s | 78 MiB | 37 MiB | 0 | 604 MiB | 12.6 M | 12.6 M |
| k1 (kernel, seed codegen) | 3.34 s | 231 MiB | 212 MiB | 202 MiB | 3.80 GB | 101.5 M | 96.4 M |
| k2 (kernel, kernel codegen) | 2.61 s | 1622 MiB | 3458 MiB | 3458 MiB | 3.80 GB | 100.7 M | 21.1 M |

k1 and k2 produce byte-identical IR. Gate: PASS (`tools/kernel_gate.sh`;
churn 3.99 GB / 101.5 M calls — up from the 3.00 GB recorded in HANDOFF
Round 13: the R5 hoist port and the diagnostics work added ~1 GB churn).
`cargo test --workspace`: 332 passed, 0 failed, 1 ignored.

Generated programs (`bench/scripts/run_all.sh --quick`): the seed is at
parity with Rust/Zig on all eight programs in time and peak RSS.
Kernel-generated code: parity on fib_rec/mandelbrot, **6.4× slower on
str_bytes**, 35% slower on fib_iter.

### 1.3 Finding: the kernel's "peak live" is leakage, not retention **[fact]**

`main` ends in `(exit (dispatch a))`; by the time `exit` runs, `dispatch`
has returned and every owned value inside it should have been dropped.
So **live-at-exit = leaked**. For k1 that is 202 of 212 MiB — 95% of the
peak. HANDOFF's "never-freed 5.5%" counted *blocks*; the leaked blocks are
the large ones. The kernel's true working set is of the order of the
seed's (tens of MiB). Consequence: **arenas are not the first lever for
compiler peak memory** — they would hide planner leaks, not fix them, and
the same leaks hit every Cato program.

Root-leak analysis (`ALLOC_SITE_ROOTS=1`, k1 self-compile; retained bytes
credited to the block where ownership was dropped):

| retained | roots | where ownership was lost | class |
|---|---|---|---|
| 46.6 MB | ~9.8 k | `sb-str` drain loop (`strbuf.cato:48-71`), via fn-text / emit-program / md-mint-str | **L: loop/recur** |
| 10.8 MB | 91 k | reader output in `mo-load-requires` | open |
| 10.7 MB | 4 | one graph from `lower-node` | open |
| ~45 MB | ~40 k | `clone__Asts`/`clone__Ast` requested by lowering & forms | open (copy + drop gap) |
| 8.2 MB | 512 k | strings from `mo-alias-sid` → `stab-get` | **F: FFI ownership** |
| 8.1 MB | 10 | `md-decl`/`md-ensure` tables | open |
| 7.9 MB | 4.8 k | `clone__HExprs` in `bcheck-node` | open |

k2 frees only 21% of its allocations: the kernel's own planner
(`moveck.cato`) still lacks the seed's R1–R6 decision logic (HANDOFF
"Remaining (the port)").

### 1.4 Finding: kernel CPU is memory management **[fact]**

`sample` of a k1 self-compile: **65%** of samples in malloc/free,
**23%** in clone/drop glue (`clone__TyEnv`+`drop__TyEnv` ≈ 14% alone). The
5.8× seed/kernel time gap is copying, not algorithms.

### 1.5 Confirmed correctness issues **[fact]** (each reproduced)

| id | issue | evidence | severity |
|---|---|---|---|
| C1 | **Use-after-free**: a boxed loop variable passed to an owned parameter is freed by the callee; the loop keeps reading it. Accepted by the checker. | ASAN heap-use-after-free on `scratchpad/probe/s1.cato`; cause: loop vars are "Copy" (`ownership.rs:851-852`, `loop_copy` in `move_if_non_copy` `:814-818`) | memory safety |
| C2 | **Silent miscompile**: `(recur (+ i 1) b a)` swapping two vector loop variables prints 22, expected 21. `recur` stores arguments one at a time (`exprs_flow.rs:383-415`) while collection loop vars are read live from their slot (`:186-191`). | `probe/sw.cato` | wrong output |
| C3 | `(reduce + x0 xs)` with an `f64` variable `x0` types as `i64` (type chosen from the *syntax* of `init`, `lower.rs:993-998`) → invalid IR, clang error. | `probe/t1.cato` | type soundness |
| C4 | A defn named `is-null?` emits `define i1 @null?(…)` unmangled → invalid IR. | `probe/p1.cato` (original) | crash, not diagnosed |
| C5 | `ty_of(Call)` falls back to `i64` for an uncached instantiation (`lower.rs:1018`); `ClosureCall` likewise (`:1044`). | **[hypothesis]** no failing probe yet | type soundness |

Stale instruments found (each fixed or recorded): the bench harness
invoked the kernel with the removed `build` syntax, so the kernel column
had silently been empty (fixed: `bench/scripts/bench.py`); `CATO_PLAN_LOG`
is documented in `ownership.rs:5-7` but not implemented; HANDOFF's
`/tmp/alloc_live.c` no longer existed (rebuilt as `tools/alloc_live.c`).

### 1.6 Leak classes in generated programs **[fact]** (`tools/leak_site.sh`)

| probe | shape | leaked per iteration |
|---|---|---|
| p2 walk-a | `(loop [l l0] (match l (CCons [h t] (recur t …))))` | the cons cell + its `h` |
| p2 walk-b | same walk by *recursion* | nothing — clean |
| L1 | `(let [s (str …)] (recur …))` | `s` |
| L2 | `(match (mk i) (P [a b] (recur …)))` | the box + its string |
| L3 | `(let [p (mk i)] (match p (P [a b] (recur …))))` | the box + its string |
| L4 | string loop var replaced by `recur` | every old value, and the last |
| L5 | box loop var replaced by `recur` | every old value, and the last |
| F | extern returning an owned `str` (`cato_stab_get` strdups, `runtime.c:288-293`) | every result: externs are never fresh (`ownership.rs:548-552`) |

Root causes, both explicit in the code: `exprs_flow.rs:468` — *"Scope-exit
frees run only on the completing path — a `recur` leaves this scope and
must not release anything"*; `ownership.rs:2186` — `arm_exit_plan`
returns `Err("recur")`; loop variables "ride as Copy … no scope frees
them" (`ownership.rs:886-893`). v2 already *specified* the right rule
(`recur` rebind drops the old value; v2 §5.1 table) — it was never built.

### 1.7 Cognitive complexity in the current model **[fact]**

1. **A special case that is unsound.** Loop variables are the one place a
   non-Copy value is silently treated as Copy (C1). The rule a reader
   must hold ("loop variables are different") is both surprising and
   wrong.
2. **Callee annotations decide moves.** Whether `(f x)` consumes `x`
   depends on `f`'s `&` annotations. The safe reflex is to copy: the
   kernel has **1,677 `(clone …)` calls in 1,230 functions**, and
   `(mod-files (clone st))`-style "clone the whole state to read one
   field" is common. Copies are the kernel's dominant CPU and churn cost
   (§1.4).
3. **`str` is Copy but copied on every store** (constructor fields,
   vector elements strdup), and has no length. Hidden O(n) work.
4. **Restrictions that leak implementation detail**: "match scrutinee
   must not be a `let`/`do` form" (`ownership.rs:1218-1235`); vectors of
   ADTs do not exist, so programs build cons lists (one `malloc` per
   element — the `StrCell` design behind the sb-str leak).
5. **No pointers and no FFI ownership vocabulary**: C cannot hand Cato a
   buffer it owns, nor can Cato express an address.

### 1.8 Strengths to preserve **[fact]**

Persistent collections with compiler-proven in-place update (unobservable
optimization, v2 §3.3); monomorphized, null-tolerant drop glue; the move
checker (E301) as the one user-facing ownership error; call-scoped `&`
lending with no lifetimes; boxing only on recursion cycles (inline
everything else); stream fusion; the interpreter as differential oracle;
the self-hosting fixpoint gate. Seed-generated code already matches
Rust/Zig on the benchmark set — performance must be preserved, not chased.

### 1.9 Reproduction

```
cargo build -p cato-cli --release && tools/mem_bench.sh /tmp/mb
tools/kernel_gate.sh
clang -dynamiclib -O2 -o /tmp/site.dylib tools/alloc_site.c
# frame-pointer k1 (generated IR has no "frame-pointer" attribute):
target/release/cato compile selfhost/cato/cli.cato --emit-ir -o k1.ll
sed -E 's/^(define .*\)) \{$/\1 #99 {/' k1.ll > k1fp.ll
echo 'attributes #99 = { "frame-pointer"="all" }' >> k1fp.ll
clang -O2 -w k1fp.ll runtime/runtime.c -o k1fp
DYLD_INSERT_LIBRARIES=/tmp/site.dylib ALLOC_SITE_ROOTS=1 ALLOC_SITE_OUT=/tmp/k1 \
  ./k1fp compile selfhost/cato/cli.cato --emit-ir -o /dev/null   # → /tmp/k1.roots
tools/leak_site.sh probe.cato           # leaks of one program
```

---

## Part II — Competing memory models

Each candidate is judged on the same Cato code. The running example:

```clojure
(deftype Cell (CNil) (CCons [h :- str t :- Cell]))
(defn drain [l0 :- Cell] :- (Vector str)
  (loop [acc :- (Vector str) [] l :- Cell l0]
    (match l
      (CCons [h t] (recur (conj acc h) t))
      (CNil [] acc))))
```

### Model A — Affine values, scope drops, call-scoped lending (v2, completed)

*Programmer knows:* non-Copy values have one owner; storing, returning,
rebinding (`let`, `recur`) or passing to a non-`&` parameter hands the
value over; the old name is then gone (E301). Values die when their
owner's scope ends.
*Compiler proves:* no use after hand-over; drop placement; in-place
update at last use.
*Example:* `drain` is accepted. Each iteration's cell is freed at the
`recur` (its `t` field skipped — it moved into `l`); `h` is copied into
`acc` (strdup) and freed with the cell.
*Costs:* zero runtime overhead beyond the frees; compile-time is one
dataflow pass. *Remaining burden:* item 2 of §1.7 (annotations decide
moves → clones).
*Cycles:* impossible to build with immutable ADTs (no mutation, no
pointers) — a graph needs indices or (Model D) pointers.

### Model B — Regions (MLKit-style inference or explicit `with-region`)

*Programmer knows:* values live in regions; a region is freed wholesale
at the end of its lexical scope; a value cannot outlive its region.
*Example:* `drain`'s cells live in the caller's region; nothing is freed
per iteration. Fine for a compiler pass (allocate the AST in one region,
drop it after lowering); bad for long loops in a long-lived region —
`(loop … (recur (conj acc …)))` in an outer region accumulates every dead
intermediate until the region dies (MLKit's well-known space leaks).
*Compiler proves:* region escape (type-and-effect inference).
*Costs:* bump allocation is the fastest possible; frees are O(1) per
region. Inference is complex, error messages are about regions — a new
concept in every signature, contrary to Cato's "no lifetimes" decision.
*Verdict:* excellent as an *allocation strategy* for phase-shaped data,
poor as the *semantic model*.

### Model C — Reference counting with reuse (Perceus / Lean 4 / Koka)

*Programmer knows:* only values. No moves, no E301, no `&`, no `clone`.
*Compiler:* inserts inc/dec, infers borrowed parameters, reuses a
uniquely-referenced cell's memory for the next constructor
("functional but in place"); `drain` runs with no allocation at all for
the cells it consumes.
*Costs:* a refcount word per object; inc/dec on every share (atomic if
shared across threads — Cato has `parallel`); uniqueness checks at
runtime (a branch per reuse site); cycles leak without a collector or weak
references; FFI must respect refcounts. Compile-time: borrow inference
plus reuse analysis.
*Verdict:* the simplest programmer model, but it trades Cato's
deterministic, check-free code for per-share runtime work, and the whole
existing planner/move-checker investment becomes redundant. It also
cannot offer "this costs nothing" guarantees for raw pointers.

### Model D — Model A + inferred lending + explicit FFI ownership + raw pointers + arena library (**chosen**)

Model A's semantics, with the four burdens of §1.7 removed:

1. **`recur` is a scope exit** (fixes C1, C2, L1–L5): loop variables are
   ordinary owned bindings; at each `recur`, everything owned by the
   iteration (lets, matched boxes, the old loop values not handed to the
   next iteration) is dropped after the new values are computed and
   before the jump.
2. **Parameter modes are inferred** (removes most clones): a parameter
   the body only reads is lent automatically; `&` remains as a checked
   promise. Moving into a lent parameter no longer ends the caller's
   ownership.
3. **FFI ownership is declared**: extern results are borrowed by
   default (never freed by Cato); `:- (own T)` marks a result the C side
   hands over (`malloc`ed; Cato frees it).
4. **`Ptr` is a Copy address**: never freed implicitly; explicit
   `ptr-alloc`/`ptr-free`, loads/stores, arithmetic; passes to C as is.
5. **Arenas are a library allocation strategy** (Model B's strength
   without its semantics): `Arena` is an owned value; memory obtained
   from it is `Ptr`; dropping the arena frees everything in it.

| criterion | A | B | C | **D** |
|---|---|---|---|---|
| rules to learn | 4 + `&` + loop exception | regions in every signature | none | **5 short rules (§III.1)** |
| runtime overhead | none | none (bump) | RC per share, uniqueness branches | **none** (opt-in arenas) |
| per-iteration reuse | via free+malloc | none until region dies | in place | free+malloc; in-place for collections |
| cycles | impossible | region-wide | leak | **via `Ptr`/indices, explicitly** |
| FFI | scalars/str only | hard | refcount friction | **declared ownership, raw pointers** |
| fits existing code | yes | rewrite | rewrite planner | **yes, staged** |
| compile cost | 1 pass | inference | inference | 1 pass + param-mode fixpoint |

Model D wins because it keeps the property that makes Cato fast today
(no runtime bookkeeping; seed code already matches Rust) while removing
the cost that makes Cato code *hard to write well* (clones). C's
simplicity is attractive, but its costs are paid on every share in every
program, and the evidence says Cato's problem is incomplete planning and
over-copying — both fixable statically.

---

## Part III — Target architecture

### III.1 Language semantics (the whole programmer-facing model)

1. **Copy or owned.** Scalars, `bool`, `str`, and `Ptr` are copied.
   Collections, ADT values and closures have exactly one owner at a time.
2. **Hand-over.** A value is handed over when it is stored (constructor
   field, collection element, closure capture), returned, rebound
   (`let`, `recur`), or passed to a parameter that keeps it. The old
   name is then gone (E301). A parameter that only reads its argument
   lends it: the caller keeps it. `(clone x)` makes a second, independent
   value.
3. **Scope death.** An owned value is freed when its owner's scope ends:
   a `let` body, a `match` arm, a function body, or a loop iteration
   (`recur`). Freeing memory has no observable effect, so the compiler
   may free earlier (at last use) or reuse the storage in place.
4. **Values, not places.** `conj`/`assoc`/`disj` return new values; the
   original is unchanged. When the original is dead, the compiler updates
   it in place.
5. **Pointers are addresses.** A `Ptr` carries no ownership. Memory you
   obtain with `ptr-alloc` (or from C with `(own Ptr)`) you free with
   `ptr-free`, or inside an `Arena` that frees it wholesale. Loads,
   stores and arithmetic are explicit operations; nothing else touches
   pointer memory.

What the compiler guarantees: for programs without `Ptr` operations, no
use-after-free, no double free, no leak on any path the planner models
(an unmodelled shape is a planner limitation recorded as a leak, never a
free — "uncertainty leaks, it never frees"). With `Ptr`, the guarantees
cover owned values; pointer memory is the programmer's (as in C).

### III.2 Required examples (target semantics)

```clojure
;; 1–2. create, mutate, grow a vector in place
(let [v (conj (conj [] 1) 2)]        ; in place: [] literal copied once, then grown
  (count v))                          ; v freed at let exit

;; 3. return a collection: ownership moves to the caller, no copy
(defn evens [n :- i64] :- (Vector i64)
  (loop [i :- i64 0 acc :- (Vector i64) []]
    (if (>= i n) acc (recur (+ i 2) (conj acc i)))))   ; conj in place each step

;; 4. capture a collection in a closure: the closure owns it
(let [xs (evens 10) f (fn [k :- i64] :- i64 (+ k (count xs)))]
  (f 1))                              ; xs moved into f's env; freed with f

;; 5. return a pointer to allocated storage (caller now responsible)
(defn make-buf [n :- i64] :- Ptr (ptr-alloc (* n 8)))

;; 6. pass a pointer to C
(extern memset [p :- Ptr c :- i64 n :- i64] :- Ptr)
(let [b (make-buf 16)] (do (memset b 0 128) (ptr-free b)))

;; 7. deterministic resource release: ownership from C, freed at scope end
(extern cato-slurp-owned [path :- str] :- (own str))
(let [text (cato-slurp-owned "in.txt")] (count text))  ; freed at let exit

;; 8. a graph with shared references: indices into a vector (safe) …
(deftype Graph (G [edges :- (Vector i64) starts :- (Vector i64)]))
;; … or raw pointers inside an arena (explicit, unchecked)
(let [a (arena-new 4096) n1 (arena-alloc a 16) n2 (arena-alloc a 16)]
  (do (ptr-store n1 0 n2) (ptr-store n2 0 n1) 0))     ; cycle; arena frees both

;; 9. many temporary compiler nodes: owned trees, dropped by scope;
;;    phase-shaped data may live in an Arena (allocation strategy only)

;; 10. reuse a buffer across repeated operations: the accumulator is
;;     rebound by recur and grown in place; the old value is never copied
(loop [i :- i64 0 buf :- (Vector i64) []]
  (if (= i 100) (count buf) (recur (+ i 1) (conj buf i))))
```

Status: 1–4, 7 (with today's `str` externs), 9, 10 are expressible now
and become leak-free with Stage 1/2; 5, 6, 8 (`Ptr`, arenas, `(own T)`)
are Stage 4–5 additions.

### III.3 Compiler analyses

- **Drop planning** (`ownership.rs`, `moveck.cato`): one forward pass
  over typed HIR; per-binding `owns`/state; drops at scope exit,
  branch-local frees for path-dependent moves, arm-exit frees for
  matched boxes, and — new — a per-`recur` drop list and loop-exit drops.
- **Loop ownership** (new): a loop variable *owns* across iterations iff
  its seed is an owned value and every `recur` argument in its position
  is fresh, itself, a moved owned local, or a box child moved out of an
  owned scrutinee. Otherwise it is a non-owning binding (never freed —
  a leak, never a free).
- **Parameter-mode inference** (Stage 3): fixpoint over the call graph;
  a parameter is *lent* iff the body never hands it over. Must be
  computed before drop planning of callers. Reproduces `&`'s checks.
- **In-place update**: unchanged (last-use counting).

### III.4 HIR changes

- `Recur(args)` gains the planner's drop list: `Recur(args, drops)`
  where `drops` names the bindings to free after the arguments are
  evaluated and before the jump. Arm boxes whose arm ends in `recur` keep
  using `HArm::exit_free`; the backend runs pending arm exits at the jump.
- Loop exits reuse the existing empty-binds `Let([], value, frees)`
  wrapper (the branch-free carrier) for loop variables to drop on the
  completing path.
- No representation change to AST/HIR node layout is needed for the
  memory wins measured so far; interning/flattening (Part III.C of the
  mandate) is deferred until the leak floor is reached and the true
  working set can be measured (§1.3).

### III.5 Runtime changes

- Stage 2: `(own T)` results freed with `free`; runtime functions that
  return owned memory are declared `(own …)` in kernel sources.
- Stage 5: `ptr-alloc`/`ptr-free`/`ptr-load`/`ptr-store`, `Arena`
  (`arena-new`/`arena-alloc`/drop) — a bump allocator in `runtime.c`.

### III.6 Self-hosting implications

Every planner rule must exist twice (seed `ownership.rs`, kernel
`moveck.cato`) and every backend rule twice (`cato-backend-llvm`,
`emit/*.cato`). The kernel already lags the seed by R1–R6 (k2 leaks
3.4 GB). Order: land each rule in the seed first (k1 benefits
immediately — k1 is seed-planned), keep the fixpoint gate green, then
port. Source changes to kernel code must stay within the kernel subset;
`(own T)` is a pure annotation the seed must accept before kernel
sources use it.

---

## Part IV — Implementation plan

| stage | content | files | gates | acceptance |
|---|---|---|---|---|
| **1** | `recur` drops; affine loop vars; evaluate-all-then-store; loop-exit drops | `hir/mod.rs` (Recur), `hir/ownership.rs`, `cato-backend-llvm/.../exprs_flow.rs`, `adt.rs`, interp, all `Recur` matches | cargo test; gate; ASAN on probes L1–L5, s1 (now rejected), sw (21) | probes leak-free; k1 peak live drops by ≳40 MB; no runtime regression in bench |
| **2** | `(own T)` extern results | `hir/parse.rs`, `ownership.rs` (`compute_alias_ret`), kernel `lib/sym.cato`, `lib/driver.cato` | same | `mo-alias-sid` roots gone |
| 2b | C3, C4 type/mangling fixes | `lower.rs`, LLVM mangler | regression tests | probes compile and run |
| **3** | parameter-mode inference | `ownership.rs` + sigs, kernel port | same + clone-count drop | k1 churn and time down |
| **4** | kernel port of R1–R6 + Stage 1–2 | `moveck.cato`, `emit/*.cato` | gate fixpoint; kernel_hir | k2 frees ≈ k1 frees |
| **5** | `Ptr`, arenas | `hir`, both backends, `runtime.c` | new tests, ASAN | examples 5, 6, 8 compile |

Rollback rule for every stage: a gate failure, a `cargo test` failure, an
ASAN finding, or a benchmark regression > 5% beyond noise reverts the
stage. Each planner rule gets a `CATO_NO_*` bisection flag like the
existing ones.

Not pursued (with reason): mandatory GC (anti-goal); RC as the default
(§II, Model C); region inference (§II, Model B); arenas for compiler
peak memory before the leak floor is reached (§1.3); AST flattening
before the true working set is known.

---

## Part V — Evidence log

(Appended per stage as results land.)

### Stage 1 — `recur` is a scope exit (landed in the seed, 2026-10-19)

Changes: `HExpr::Recur(args, drops)`; loop variables are owned bindings
(the `loop_copy` exception is gone); `loop_owning` decides per variable
whether it owns on *every* iteration; per-`recur` drop lists guarded by a
may-alias roots analysis; loop-exit drops on completing leaves; arm-exit
frees run at `recur` (backend `pending_exits`); all `recur` arguments are
evaluated before any loop slot is stored; moves on always-`recur` paths
stay out of merges but reach enclosing arm plans (`div_moved`); a `let`
bound to a loop owns its value when every completing leaf is owned.
Bisection flag: `CATO_NO_LOOPDROP`.

| metric (k1 self-compile, `tools/mem_bench.sh`) | before | after |
|---|---|---|
| peak live | 212 MiB | **133 MiB** |
| live at exit (leaked) | 202 MiB | 122 MiB |
| max RSS | 231 MiB | 187 MiB |
| wall | 3.34 s | 3.40 s (noise band) |
| churn / allocs | 3.80 GB / 101.5 M | unchanged (only frees added) |

Correctness: C1 now rejected (E301, `examples/errors/loop_var_moved.cato`);
C2 prints 21. Probes p1–p4, swap, nested-recur (the `read/scan` shape that
crashed the first attempt) and 11 adversarial cases: correct output, ASAN
clean at -O0, zero leaks except documented conservative fallbacks (`&T`
call / `let` alias of a binder handed to `recur`, a shadowed loop
variable, an outer alias, a seed folded to a static literal). Gate PASS
(fixpoint); `cargo test --workspace` 332 passed / 0 failed. Benchmark IR
is unchanged by the stage (none of the eight programs had the leaking
shapes). Seed compile cost: +4% allocations (roots-map clones).
Follow-ups: promote static loop seeds to owned copies; restore shadowed
state after `let` bodies (would let `binds_anywhere` relax); port to
`moveck.cato` + kernel emitter (k2 unchanged at 3.4 GB).

### Type system for performance (integers, vectors) — landed in the seed

Soundness first (each confirmed before its fix):

| id | bug | fix |
|---|---|---|
| C3 | `reduce` accumulator typed from the *syntax* of `init` → invalid IR for an f64 variable init | `HReduce::acc_ty`, decided once at lowering from the init's real type |
| C7 | a hoisted reduce helper typed every capture/base vector that was not a defn parameter by guessing (`f64` / `(Vector i64)`): a `let`-bound i64 capture became a `double` parameter (invalid IR); with narrow vectors it would have been a silent wrong-width load | the reduce rewrite threads a typed `Scope` (`let`, `loop`, `for`, match binders via the ADT table, lambda captures/params); an unknown type is an internal panic, never a guess |
| C8 | **calls were not checked against their callee**: wrong arity compiled (reading an undefined register), wrong argument types reached the backend (`(f 5)` for a `str` parameter crashed at run time) | `check_call_args`: arity (E201) and type (E202) for every concrete user/extern call |
| C9 | `subvec` copied `start*8` bytes from the *start* of the source: wrong elements, and a heap-buffer-overflow (ASAN-confirmed) whenever `start > end-start` | copy `n` elements from `ptr + start` |
| — | `let`/`loop` annotations knew 5 type names, parameters 16 (`(loop [b :- u8 …])` was "unknown type") | one table, `builtin_ty_name` |
| — | `x :- &(Vector T)` was documented but parsed as an empty type name | parsed |

The call checks immediately found **six latent type errors in the kernel
source**, all fixed: `expand/asts-null` and `ast/asts-first` applied to
`HExprs` (the latter took the list by value and dropped its tail with
`Asts` glue), `arms-rev` seeded with the wrong nil and `arm-labels-mint`
with another (swapped, three sites), `ctor-finish` handed a `TyEnv` where
the expected `Ty` belongs (generic ctors inferred type arguments from the
wrong structure), the arm-exit emitter calling `aef-fields` without the
variant (field offsets from garbage), and a stale extra argument in the
stdlib driver.

Expressiveness and memory:

- **Contextual numeric literals**: a literal takes the width its context
  expects (call arguments, returns, `let`/`loop` annotations, the other
  operand of arithmetic/comparison, `conj` elements, vector literals) when
  exactly representable; out of range is E202, never a wrap. `(* a 3)` for
  an `i32` `a` no longer needs `(i32 3)`. A pure relaxation.
- **Narrow-element vectors**: `(Vector i8|i16|i32|u8|u16|u32|u64|f32)`
  store at their own width (`Elem::N`), with correct element sizes and
  alignments in every buffer path (an `align 8` claim on a byte array is
  UB); `get` yields the element type; fused `reduce` widens elements
  losslessly into the accumulator (`zext`/`sext`/`fpext`/`[su]itofp`).
  Measured: 20 M elements built with `conj` and summed — **52.5 MiB vs
  410.1 MiB max RSS** for `(Vector u8)` vs `(Vector i64)`, same time.
- **Integer operators**: `rem` (truncated), `mod` (floored), `bit-and/or/
  xor/not`, `bit-shift-left/right` (arithmetic for signed, logical for
  unsigned; the amount is taken modulo the width — an LLVM shift by ≥ width
  is poison, Cato defines every shift). One semantics table,
  `hir::eval_int_binop`, shared by constant folding and the interpreter;
  the LLVM emitter renders the same table. Verified three ways (native
  -O0/-O2, `--no-opt`, interpreter) against an independent reference.

Gates: `cargo test --workspace` 338 passed / 0 failed (new:
`examples/intops.cato`, `examples/narrow_vectors.cato`,
`examples/mem/loop_drops.cato` in both parity suites, exact-output tests,
ASAN at -O0, a macOS leak test, element-width IR checks, four new
error-suite cases); kernel gate PASS (fixpoint) — run with
`CATO_GATE_TMP` (new) because another checkout ran the gate concurrently
and the fixed `/tmp` paths collided; all eight benchmark outputs match, the
seed stays at Rust/Zig parity.

Strings were measured and deliberately not changed yet: `strlen` is 0.5%
of the kernel's CPU profile and LLVM hoists it in the byte-scan benchmark,
so a length-carrying `str` (an ABI change across both compilers and the
runtime) is a correctness-of-complexity improvement (O(1) `count`, no
hand-hoisted lengths), not a measured hotspot. Recorded follow-ups:
`get` on an empty vector loads element 0 before discarding it (possible
out-of-bounds read); `(count (args))` and a closure calling `count` on a
captured `str` crash the backend; integer division by zero is LLVM UB
natively (the interpreter traps).
