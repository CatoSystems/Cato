# Cato FFI + auto-SIMD design (Phase 9, bounded slice)

## FFI

`(extern name [x :- f64 s :- str] :- i64)` declares a C symbol:

- Emitted as a plain LLVM `declare` — calls compile to direct C calls with
  **no marshaling**: i64 → `i64`, f64 → `double`, bool → `i1`,
  str → `ptr` (NUL-terminated static data), vec → `ptr` + `i64` len.
- Externs take no body and no type parameters; all params are shared
  borrows (C reads them, the caller keeps ownership).
- The compiler's own runtime declarations (`strlen`, `strcmp`) are
  minted lazily and deduplicated, so user externs and builtins share one
  symbol namespace without redefinition conflicts.

## Auto-SIMD (--fast)

`cato --fast` attaches `!llvm.loop.vectorize.enable` metadata to every
fused loop's backedge:

```
br i1 %cond, label %loop, label %ret, !llvm.loop !0
!0 = distinct !{!0, !1}
!1 = !{!"llvm.loop.vectorize.enable", i1 true}
```

Fused loops are pure (read-only `noalias` data, branchless bodies when the
pipeline is control-flow-free), which is exactly the shape LLVM's
vectorizer needs — the metadata raises the loop from "maybe" to "opt-in".
Default builds stay unchanged; `--fast` opts in.

## Deferred (9b)

- Width types `i8/i16/i32/f32/u*/usize` — requires parameterizing the
  arithmetic emitters over integer widths (every `i64` literal in
  `emit_int` becomes width-aware).
- `(parallel loop)` / auto-parallelization across threads — needs a
  runtime thread pool and ownership of the loop's write set.
