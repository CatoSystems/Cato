# Hermetic native compile: statically linked LLVM + in-process lld, with bundled-lld fallback

Date: 2026-10-19
Goal: let end users compile Cato programs to native binaries on machines
with **no clang and no LLVM installed**. The compiler emits object code
in-process and links it in-process (or via a bundled linker), producing
statically linked binaries on Linux (musl) and Windows (mingw), and
dynamic binaries on macOS (platform limitation — Apple does not support
full static linking of system libraries).

Status: design, approved for planning.

## Decisions (recorded from design dialogue)

1. **Target architecture: Approach 1** — statically link LLVM codegen
   *and* lld into the `cato` binary; the native compile/link pipeline
   runs with zero external processes. No system toolchain is consulted
   when the capabilities are built.
2. **Two separate build capabilities**, independently selectable:
   - `static-llvm` — in-process LLVM IR → object codegen.
   - `in-process-lld` — in-process linking via a C++ shim over lld's
     link API. Implies `static-llvm`.
3. **Approach 2 is an explicit fallback, not a hidden one**: when
   `static-llvm` is on but `in-process-lld` is off, `cato` links by
   extracting a bundled standalone `lld` binary and invoking it as a
   subprocess. Codegen remains in-process; only linking is external.
   The bundled `lld` executable is produced at cato-build time from the
   same LLVM build used for `static-llvm`, embedded alongside the
   per-target assets, and extracted to the cache dir on first use.
4. **Platform matrix**:
   - Linux (`x86_64`, `aarch64`): **static** binaries, **musl** libc.
   - Windows (`x86_64`): **static** binaries, mingw-w64 CRT.
   - macOS (`x86_64`, `aarch64`): **dynamic** binaries (libSystem).
5. **Cross-compilation for all targets from any host**: `cato` may
   produce binaries for any target in the matrix regardless of host OS.
   All needed assets are embedded in the binary.
6. **WASM is out of scope**: the wasm32 path keeps its current
   external-clang + wasm-ld flow unchanged.
7. **Legacy path preserved**: a build with neither feature behaves
   exactly as today (shell out to system `clang`). Existing users and
   tests are not broken.
8. **No silent degradation**: a build with `static-llvm` must never
   fall back to system `clang`. Missing assets fail fast with an
   explicit error instead.

## 1. Build capabilities

Cargo features (workspace/`cato-cli`):

| Feature | Effect |
|---|---|
| *(none)* | Today's behavior: temp `.ll`, system `clang` compiles+links. |
| `static-llvm` | In-process codegen: `Program` → LLVM IR → object in memory. Link via bundled `lld` subprocess (fallback). Never touches system `clang`. |
| `in-process-lld` | Adds in-process linking via C++ shim over `lld::elf/coff/macho::link`. Implies `static-llvm`. Zero external processes. |

Distribution builds enable both (Approach 1). CI builds the full matrix
(`none`, `static-llvm`, `static-llvm+in-process-lld`) so every path has
continuous coverage.

`static-llvm` uses `llvm-sys`'s static linking (or equivalent) to pull
LLVM codegen libraries into the `cato` binary. `in-process-lld` adds a
new thin C++ shim crate (working name `cato-lld-sys`) that wraps
`lld::elf::link`, `lld::coff::link`, and `lld::macho::link` behind a
stable `extern "C"` surface. Building `cato` with `in-process-lld`
requires a C++ toolchain and per-host prebuilt LLVM+lld static libs;
building *with* these features is a maintainer concern only — end users
receive prebuilt binaries.

### Capability resolution (per compile, decided once)

1. `in-process-lld` compiled in → in-process link.
2. else `static-llvm` compiled in → in-process codegen + bundled-lld
   subprocess link.
3. else → legacy system-clang path (unchanged).

## 2. Embedded per-target assets

All assets are produced at **cato-build time** (CI/dev) and embedded
via `include_bytes!` (or an equivalent data section), keyed by target
triple:

- **`runtime.o`** — `runtime/runtime.c` cross-compiled per triple:
  `musl-gcc`/`zig cc` for musl targets, `x86_64-w64-mingw32-gcc` for
  Windows, Apple clang for macOS targets.
- **musl static libc** (Linux targets): `libc.a` + CRT startup
  (`crt1.o`, `crti.o`, `crtn.o`) per triple.
- **mingw-w64 static CRT** (Windows target): `libmingw32.a`,
  `libmingwex.a`, `crt2.o`, and companions per triple.
- **macOS `.tbd` stubs**: `libSystem` and the other stubs needed at
  link time, per triple. The link remains dynamic; only stubs (tiny
  text files) are embedded — no full SDK.

A build-time check (build script or test) fails the *cato build* if any
target in `TARGETS` lacks its asset set, so an embedded-asset gap can
never reach a release binary.

## 3. Data flow (hermetic native compile)

For `cato compile prog.cato -o prog --target x86_64-linux` with both
features on:

1. Frontend unchanged: parse → typecheck → optimize → `Program` (HIR).
2. In-process codegen: `Program` → LLVM IR (as today) → object emitted
   **in memory** (no temp `.ll`, no `clang -c`).
3. In-process link: in-memory object + embedded `runtime.o` + embedded
   musl `libc.a`/CRT → final executable written to `prog` via
   `lld::elf::link` in the same process.
4. Output written directly to the `-o` path.

Cross-compilation falls out of this: the triple selects the flavor
(`elf`/`coff`/`macho`), the `runtime.o` variant, and the libc/CRT
assets. `resolve_cc`, `cc_supports_triple`, and `resolve_wasm_cc`'s
native half are skipped entirely when `static-llvm` is on; they remain
only for the legacy path (and wasm, unchanged).

WASM continues through the existing external flow.

## 4. Error handling

- **lld failure (in-process)**: diagnostics come back from the C++
  shim as a string; surfaced as the existing `Err(String)` from
  `build_native`. No exit-status decoding.
- **lld failure (bundled subprocess)**: capture child stderr + status,
  wrap as `Err(String)` — same shape as today's `run_cc`.
- **Missing embedded asset for a requested target**: fail fast:
  `cato: no embedded assets for target '<t>' (this cato build does not
  embed <t>)`. Never fall through to the system-clang path.
- **Bundled `lld` binary missing/corrupt (fallback path)**: explicit
  error suggesting reinstall of `cato`. Never degrade to system clang.
- **Extraction races**: the bundled `lld` is extracted to the user
  cache dir (`~/.cache/cato/lld-<version>/<triple>/lld`, platform
  equivalent on Windows/macOS) once, guarded by a lock file.
- **In-process path needs no temp files** for `.ll`/`.o`; output is
  streamed to the `-o` path only.

## 5. Testing strategy

**Unit (no LLVM/lld required, run on every platform):**
- Asset lookup: correct per-triple keys; missing target → explicit
  error.
- Capability resolution: feature-flag combinations select the right
  path; `static-llvm` never reaches system clang (regression guard).
- Link input assembly: the in-process path resolves runtime object,
  libc assets, output path, and flavor correctly (assert on the shim's
  input struct, since there is no argv).

**Integration (feature-gated CI jobs):**
- `--features static-llvm,in-process-lld`: compile fixtures end-to-end
  per embedded target; assert output exists, correct format/headers;
  for host-platform targets, *run* the output and compare stdout to
  expected fixtures.
- Static guarantee: Linux outputs have no dynamic deps (`readelf -d`);
  macOS outputs depend only on `libSystem` (`otool -L`).
- Fallback: build `static-llvm` only; assert bundled-lld output is
  behaviorally equivalent to the in-process output.
- Legacy: no-feature build stays green with the existing suite.

**Cross-compile smoke (single CI host, all features on):** compile a
fixture for every target in the matrix; assert file format sanity
(`file`/`readelf`/`objdump`) even where the binary cannot be executed.

**Known risk to measure:** a much larger statically linked `cato`
binary may increase process startup time; watch that it does not offset
the link-time savings from dropping the subprocess roundtrip.

## 6. Out of scope

- WASM hermeticity (unchanged external flow).
- Kernel (`selfhost/cato/cli.cato`) parity — this spec covers the seed
  CLI only; kernel adoption is a separate workstream.
- Making `runtime.c` a full platform-aware runtime library (static/
  shared, debug/release variants) — noted by the CLI-contract spec as a
  separate track; this spec only requires per-triple `runtime.o`.
- Shipping `cato` itself as a fully static binary (the compiler's own
  linking is a separate concern; macOS system dylibs apply there too).
