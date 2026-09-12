# Cato Collections + Macros design (Phases 4 & 5)

## Phase 4 — Collection API

The invariant: every collection operation below compiles to **one
allocation-free loop**. Nothing materializes intermediates.

### Fused loop sources (`HSource`)

```
(reduce <combine> <init> <source>)
  source := var                        ; vector variable
          | (vec-i64 ...) | [1 2 3]    ; inline literal base
          | (range [start] end)        ; generated elements, no backing vector
          | (zip var var)              ; two vectors read in lockstep
          | (take n source)            ; bound-adjusting wrappers
          | (drop n source)
          | (map fn source) | (filter fn source)
```

- Loop helpers take one `data/len` pair per base vector (`range` takes none —
  its bounds are ordinary captures evaluated branchlessly in the entry block).
- `take`/`drop` compile to arithmetic on a start-index/count pair
  (`s' = s + d`, `n' = max(0, n - d)`, `n'' = min(n', k)`) — `select`, never
  branches, so the loop shape is unchanged.
- `zip` binds a **second element variable**: map/filter lambdas and reduce
  closures over a zip source take two element params (`(fn [a b] ...)`,
  `(fn [acc a b] ...)`). Count is `min(lenA, lenB)`.
- The loop reads elements at `start + i`; `range` binds the element directly
  to `start + i` without a load.

### `get`

`(get vec idx)` / `(get vec idx default)` — a runtime-bounds-checked element
load emitted **branchlessly**: an `icmp ult` bounds test, a clamped
`inbounds` GEP, one `align 8` load, and a `select` for the default
(no default ⇒ out-of-bounds yields 0).

### Deferred from Phase 4

`conj` / `subvec` return vectors, which requires either heap allocation or
the ownership/borrow story (Phase 7) to express safely. Deferred deliberately
to keep the zero-allocation invariant honest; they land with the escaping-
value story.

## Phase 5 — Macro expansion beyond threading

All new forms desugar in `expand.rs` into core forms (`if`/`let`/`do`/
`reduce`/comparisons) — zero codegen changes:

| Macro | Expands to |
|---|---|
| `(when c b...)` | `(if c (do b...) 0)` |
| `(when-not c b...)` | `(if c 0 (do b...))` |
| `(cond c1 b1 ... [else])` | nested `if`s; odd trailing form is the default, else 0 |
| `(if-let [x e] t [f])` | `(let [x e] (if x t f-or-0))` — `e` must be `bool` |
| `(when-let [x e] b...)` | `(let [x e] (if x (do b...) 0))` |
| `(if-not c t f)` | `(if c f t)` |
| `(doseq [x xs] b...)` | `(reduce (fn [_acc x] (do b... 0)) 0 xs)` |
| `(and a b c...)` / `(or ...)` | nested binary forms (1 arg → itself) |

The `0` fill for "no value" mirrors Clojure's `nil` with the language's
unboxed i64 default. Expansion recurses into binding vectors so macros work
in `let`/`doseq` value position; `deftype` forms are untouched.

`for` (vector-building comprehension) is deferred with `conj` — it needs
vector materialization.

## Reference program

`examples/collections.cato` → `35 9 140 64 42 3 5 10 30`.
