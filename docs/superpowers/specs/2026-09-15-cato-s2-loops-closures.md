# S2 — `loop`/`recur` + rich closures

Date: 2026-09-15
Goal: general iteration and closures that capture anything — the control
floor the self-hosted kernel's passes stand on. Reference:
`examples/loops.cato`.

## 1. `loop` / `recur`

- `(loop [x :- i64 0 acc :- i64 1] body)` — bindings are `name :- ty init`
  groups; the type ascription is **required** (the codegen allocates one
  slot per variable; there is nothing to infer from). The body is an
  implicit `do`; the loop's value is the body's value on the completing
  path.
- `(recur e1 ...)` re-binds the loop variables and jumps to the loop head.
  It targets the **innermost enclosing loop**; arity must equal the loop's
  binding count; it must appear in **tail position** — the loop body tail,
  `if` branches, `match` arm bodies, `let`/`do` tails. Anywhere else is a
  compile error (validated post-lowering by a tail walk). `recur` crossing
  a closure boundary is an error (fn-head `recur` is not in this slice).
- Loop variables are **Copy types** in this slice: numeric widths, `bool`,
  `str`. Accumulators that move (boxed ADTs, vectors) come later; the
  ownership checker stays exact — `recur` arguments are non-moving reads of
  Copy values, so the once-per-body check remains sound.
- LLVM: the classic slot compilation — one alloca per variable (hoisted),
  inits stored before the head; each iteration loads the variables into
  fresh registers at the head; `recur` evaluates its arguments in the
  current block, stores them into the slots, and branches to the head; the
  completing path branches to the merge block and phis completing values.
  Branches that end in `recur` are terminating: they contribute no phi arm.
  A loop whose body always recurs emits `unreachable` after the merge.
- Interp: a recur sink — `recur` evaluates its arguments, stashes them,
  and the enclosing loop's evaluation loop re-binds. Validation guarantees
  the nearest loop is the target.

## 2. Closures capture any value type by move

- The capture restriction lifts: vectors, maps, sets, boxed ADTs, and other
  closures capture **by move** (the ownership checker already treats
  non-Copy captures as moves). Strings stay Copy (aliasing capture).
- Env layout widens from one 8-byte slot per capture to the type's needs:
  1 slot for scalars/bool/str/boxed ADT, 3 for vec/map/set headers
  (reconstructed as growable slots inside the lambda body, so `conj` and
  friends work on captured collections), 2 for a captured closure's
  `{code, env}`. Creation and lambda-side loading share one layout
  function.

## Non-goals

fn-head `recur`; non-Copy loop variables; `set!`-style loop escapes; loop
variables with frees inside the loop (Copy-only keeps the annotation exact).
