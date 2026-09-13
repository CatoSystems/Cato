# Cato First-Class Closures Design (Phase 14)

## Goal

Lambdas become values: `(fn ...)` works anywhere an expression does,
closures capture their free locals, and higher-order user functions take
`(Fn [t1 ...] ret)` params. Fused pipelines keep their static
defunctionalized path — closures hit the new calling-convention path.

## Types

`Ty::Fn(u32)` indexes a program-level structural signature table
`(param tys, ret ty)` (deduped, so structurally identical signatures are
the same type). Annotations: `(Fn [i64 i64] i64)`. Closures are non-Copy:
passing one by value moves it; calling one reads it (the callee gets its
own copy of the header, so a param is callable repeatedly).

## Values

A closure value is a two-word header `{ code ptr, env ptr }`:
- the code pointer targets an out-of-line `@lambda_N(ptr env, params...)`
  generated per `(fn ...)` creation site;
- the env block is one malloc'd 8-byte slot per capture; captures are
  scalars or `str` in this slice (vectors are two words — rejected with a
  clear error until a later slice extends the env to untyped slots).

Captures are by value: each free local referenced by the body is read at
creation time and stored into the env. Capturing a non-Copy binding moves
it (enforced by the ownership checker).

## Lowering & codegen

- `HExpr::Closure { sig, mangled, params, captures, body }` — creation;
  the body stays in the node (the interpreter evaluates it directly) and a
  copy lands in `Program::lambdas` for out-of-line LLVM emission.
- `HExpr::ClosureCall { f, args, sig }` — `(f args...)` where `f` is a
  local of `Fn` type; emitted as an indirect `call` with the env as the
  first argument.
- Lambda bodies get the same loop-hoisting rewrite as plain defns, so
  `reduce`/`map`/`filter` inside a closure body stay fused.

## Interpreter

`Value::Closure` carries the params, capture bindings, and the body; a
call binds captures + args in a child env and evaluates the body. No
out-of-line machinery needed.

## Slice limits (documented)

- Closure params need type annotations; `fn` types cannot be returned from
  defns yet; captures are scalar/str only; `(Fn ...)` as a struct field is
  out of scope until ADT fields widen.

## Verification

`examples/closures.cato` — captures, higher-order `twice`/`compose`,
closures inside nested lets — with identical output through the LLVM
backend (clang) and the interpreter (the parity suite now covers it), plus
HIR-level capture/move tests.
