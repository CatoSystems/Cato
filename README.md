# Cato

A small, compiled, Lisp-flavored language with ownership and deterministic
memory management. Cato source (`.cato`) compiles to LLVM IR, then to a
native binary via your system C compiler. A stream-fusion pass turns
`map`/`filter`/`reduce` pipelines into single, allocation-free loops.

```cato
(defn main []
  (println (reduce + 0 (map (fn [x] (* x x)) [1 2 3 4 5]))))
```

## Build

Requires Rust (edition 2021) and `clang`.

```sh
cargo build --release
```

## Use

```sh
# compile to a native binary
cato compile examples/closures.cato -o /tmp/closures && /tmp/closures

# emit LLVM IR instead
cato compile examples/closures.cato --emit-ir

# interactive REPL
cato repl
```

## Layout

- `crates/cato-core` — frontend + middle end (backend-agnostic)
- `crates/cato-backend-llvm` — LLVM IR emitter
- `crates/cato-backend-interp` — interpreter (used by parity tests)
- `crates/cato-cli` — the `cato` binary
- `runtime/runtime.c` — C runtime helpers
- `examples/` — sample programs

See `docs/ARCHITECTURE.md` for the pipeline and `docs/STDLIB.md` for the
standard library.

## Roadmap

- [x] Core language, ownership, persistent collections
- [x] LLVM + interpreter backends, stream fusion
- [x] Self-hosting kernel (reader through emitter)
- [ ] Stage 1: kernel builds itself
- [ ] Fixpoint + CI, seed freeze
- [ ] Diagnostics, REPL/LSP, macros, more backends
- [ ] Editor integration (VS Code, Neovim, Helix, Emacs via `cato lsp`)
- [ ] First-class struct/pointer types to call C libraries directly (no FFI shims)

## Non-goals

- Compiling Clojure source directly or full Clojure interop.
- A tracing garbage collector or mandatory runtime.
