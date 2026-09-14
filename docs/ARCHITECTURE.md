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
    src/opt/              middle end: fold, DCE/copy-prop, devirtualization,
                          inlining, clone elision over the HIR
    src/lib.rs            Program + Backend trait + compile_to_program()
  cato-backend-llvm/    LLVM IR emitter (textual IR, tagged unions,
    src/llvm.rs           fused loops, vectorization metadata)
  cato-cli/             the `cato` binary: compile modes (emit IR / build /
    src/main.rs           run interpreter / run native), flags, I/O; native
    src/native.rs         linking via the system C compiler + runtime.c
runtime/runtime.c       C runtime helpers (print-*, shared with every backend)
```

## Pipeline

```
.cato source
  → cato_core::compile_to_program()
      reader → expand → hir (build_program: typing, monomorphization,
      ownership check) → fuse (fusion invariant)
      → opt::optimize (fixed point: fold → simplify → devirt → inline;
        then free-list recompute + clone elision + ownership re-verify)
  → Program { adts, defns }
  → dyn Backend::emit(&Program) → IR text
  → clang / target toolchain
```

The middle end runs on the HIR before any backend sees it, so both the
LLVM backend and the interpreter benefit. Passes are semantics-preserving
by construction and guarded by the full examples/ parity suite plus an
opt-off vs opt-on interpreter differential (`opt_parity.rs`);
`cato --no-opt` skips the pipeline and `--dump-hir` prints the result.

## Compile modes

```
cato <file.cato>              compile check: full pipeline, no output
cato <file.cato> --emit-ir    emit LLVM IR (stdout, or -o path)   [opt-in]
cato build <file.cato>        IR + runtime/runtime.c -> native binary (-o, default: input stem)
cato build --target wasm32    -> WebAssembly binary (wasm32-unknown-unknown)
cato run <file.cato>          execute with the tree-walking interpreter
cato run --native <file.cato>   build and execute a native binary in one step
```

IR emission is opt-in: the default compile mode runs the whole pipeline
(frontend, middle end, codegen) and reports errors without writing
anything — it prints a one-line `✓ compiled <file> in <n>ms` summary to
stderr. Errors are colored on a TTY (`NO_COLOR` disables that), mistyped
flags and commands get did-you-mean suggestions, and exit codes are
tiered: 0 success, 1 compile/runtime error, 2 usage error. Native linking
goes through the system C compiler (`--cc`, else `$CATO_CC`/`$CC`/clang)
with `-O` level control, `--emit-ll` to keep the intermediate IR, and
`--asan` for AddressSanitizer builds. The runtime is located relative to
the compiler binary, `$CATO_RUNTIME`, or the working directory.

The wasm32 target stamps the emitted module with the wasm32 triple and
data layout (`cato_llvm::Target`); freestanding runtime C helpers become
host imports (`--no-entry --allow-undefined`). Linking `.wasm` files needs
a wasm-capable clang with lld (`brew install llvm lld`, then `--cc`);
without one, `--emit-ir --target wasm32` still produces IR that LLVM's
wasm backend accepts (asserted via `llc` in the tests). `cato run`
(interpreter) and native execution produce identical output; both parity
suites assert it.

## Adding a backend

1. New crate `cato-backend-<name>` depending on `cato-core`.
2. Implement `cato_core::Backend` (`name()`, `emit(&Program)`).
3. Register it in `crates/cato-cli/src/main.rs`'s `--backend` dispatch.

The LLVM backend is the reference implementation; invariants every backend
must preserve live in `docs/superpowers/specs/` (unboxed scalars, fused
single-loop pipelines, `noalias` loop data, exhaustive ADT dispatch).
