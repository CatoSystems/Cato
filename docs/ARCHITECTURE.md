# Cato architecture

Cato is a Cargo workspace split so the frontend never knows which backend
will consume its output — new backends (Cranelift, interpreter, WASM, ...)
plug in without touching any pipeline stage.

```
crates/
  cato-core/            frontend (backend-agnostic)
    src/reader.rs         S-expressions → AST
    src/expand.rs         macro expansion (->, ->>, when, cond, ...)
    src/hir.rs            typed HIR, monomorphization, ownership check
    src/fuse.rs           stream-fusion validation
    src/lib.rs            Program + Backend trait + compile_to_program()
  cato-backend-llvm/    LLVM IR emitter (textual IR, tagged unions,
    src/llvm.rs           fused loops, vectorization metadata)
  cato-cli/             the `cato` binary: flags, backend selection, I/O
runtime/runtime.c       C runtime helpers (print-*, shared with every backend)
```

## Pipeline

```
.cato source
  → cato_core::compile_to_program()
      reader → expand → hir (build_program: typing, monomorphization,
      ownership check) → fuse (fusion invariant)
  → Program { adts, defns }
  → dyn Backend::emit(&Program) → IR text
  → clang / target toolchain
```

## Adding a backend

1. New crate `cato-backend-<name>` depending on `cato-core`.
2. Implement `cato_core::Backend` (`name()`, `emit(&Program)`).
3. Register it in `crates/cato-cli/src/main.rs`'s `--backend` dispatch.

The LLVM backend is the reference implementation; invariants every backend
must preserve live in `docs/superpowers/specs/` (unboxed scalars, fused
single-loop pipelines, `noalias` loop data, exhaustive ADT dispatch).
