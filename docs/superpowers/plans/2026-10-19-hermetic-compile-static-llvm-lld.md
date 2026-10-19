# Hermetic Native Compile (static LLVM + lld) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `cato compile` produces native binaries on machines with no clang/LLVM installed, by emitting object code in-process via statically linked LLVM and linking in-process via lld, with a bundled-lld subprocess as the explicit fallback when `in-process-lld` is not built.

**Architecture:** Two cargo features on `cato-cli` gate everything: `static-llvm` (in-process codegen via `llvm-sys`; link via extracted bundled `lld` subprocess) and `in-process-lld` (adds a C++ shim crate `cato-lld-sys` over `lld::elf/macho/mingw::link`; implies `static-llvm`). Per-target link inputs (runtime.o, musl/mingw CRT+libc, macOS tbd stubs) are embedded from a `cato-assets` crate. Builds with neither feature keep today's system-clang behavior unchanged; wasm32 always uses the existing external flow.

**Tech Stack:** Rust 2021 workspace, `llvm-sys = "231"` (static LLVM 23.1.x), C++17 shim over lld's `lld/include/lld/Common/Driver.h` API (`exitEarly=false`), `cc` crate for shim builds, `zig cc` + `xcrun` for asset generation, `tools/build_toolchain.sh` (CMake) for maintainer/CI LLVM+lld builds.

**Spec:** `docs/superpowers/specs/2026-10-19-hermetic-compile-static-llvm-lld-design.md`

## Global Constraints

- Cargo features on `cato-cli` (only new user-visible knobs):
  - `static-llvm = ["dep:llvm-sys", "dep:cato-assets"]`
  - `in-process-lld = ["static-llvm", "dep:cato-lld-sys", "cato-lld-sys/link"]`
- Only new external Rust dependency: `llvm-sys = { version = "231", optional = true, features = ["prefer-static", "strict-versioning"] }`. Everything else is path deps or build-time tools.
- LLVM toolchain prefix: `vendor/llvm-prefix` at the workspace root, built by `tools/build_toolchain.sh` from an LLVM 23.1.x source tree (`LLVM_SRC` env, default `~/clones/llvm-project`), with `LLVM_ENABLE_PROJECTS=lld`, `BUILD_SHARED_LIBS=OFF`, `LLVM_TARGETS_TO_BUILD=X86;AArch64`. `vendor/` is gitignored.
- `.cargo/config.toml` at the workspace root (created in Task 4):
  ```toml
  [env]
  LLVM_SYS_231_PREFIX = { value = "vendor/llvm-prefix", relative = true, force = false }
  LLVM_STATIC_STDCXX = { value = "1", force = false }
  ```
- Embedded assets live in `crates/cato-assets/assets/<target-name>/` (names match `driver::TARGETS`):
  - **Committed** (licenses permit): `x86_64-linux/`, `aarch64-linux/`, `x86_64-windows/`.
  - **Generated, gitignored**: `x86_64-macos/`, `aarch64-macos/` (Apple SDK tbd stubs — license forbids committing) and `lld/<host-target-name>/lld` (host-native bundled linker binary, ~100 MB, produced by `tools/build_toolchain.sh` and copied by `tools/gen_assets.sh`).
  - Required filenames per target, asserted by `cato-assets` build.rs: linux: `runtime.o`, `libc.a`, `crt1.o`, `crti.o`, `crtn.o`, `libcompiler_rt.a`; windows: `runtime.o`, `crt2.o`, `libmingw32.a`, `libmingwex.a`, `libmsvcrt.a`, `libkernel32.a`; macos: `runtime.o`, `libSystem.tbd`.
- Capability resolution is compile-time (`cfg`), never runtime probing of system tools:
  - `in-process-lld` → in-process link.
  - `static-llvm` without `in-process-lld` → bundled-`lld` subprocess link (cato-cli build.rs emits `cfg(cato_bundled_lld)` when it detects this combo).
  - neither → legacy system-clang path, byte-for-byte unchanged.
  - wasm32 always takes the legacy external path, even with features on.
- With `static-llvm` built in, the native path must **never** invoke or consult system clang, `CC`, `CATO_CC`, or `CATO_RUNTIME`. Missing assets fail fast; no fallback.
- Pinned error strings (tests assert these exactly):
  - Missing assets: `cato: no embedded assets for target '<t>' (this cato build does not embed <t>)`
  - Corrupt bundled lld: `cato: bundled lld is missing or corrupt (expected <n> bytes, found <m>) — reinstall cato`
  - `cato: --cc is not used in this cato build (it links hermetically; no external C compiler is consulted)`
  - `cato: --runtime is not supported in this cato build (the runtime is embedded)`
  - `cato: --asan is not supported in this cato build`
  - `cato: --rtlib is not supported in this cato build (compiler-rt is embedded)`
  - `cato: passthrough '<tok>' is not supported in this cato build`
- Hermetic codegen triples (differ from the CLI triples on purpose — musl/windows-gnu objects): `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`. The CLI `--target` names/triples in `driver::TARGETS` are unchanged.
- Hermetic linking stages all inputs into a temp directory (lld reads file paths, not buffers), links, then removes the dir on success and failure. No temp `.ll` file is written (IR stays in memory; `--emit-ll` still writes it when requested).
- macOS static linking is out of scope: macOS links stay dynamic against libSystem (spec §4).
- No new top-level documentation files; maintainer usage lives in script headers.

## Review Focus

1. **`static-llvm` silently reaching system clang** — a developer's PATH makes a missed `cfg` gate look like success. Expected: with `PATH` cleared, `cato compile examples/slice.cato` under `in-process-lld` still succeeds. Pinned by `hermetic_host_compile_runs_without_cc` (Task 6).
2. **Asset-set drift** — `gen_assets.sh` gains/loses a file that Rust argv or build.rs requires (or vice versa). Expected: build or test fails naming the file. Pinned by cato-assets build.rs required-lists + `assets_for` tests (Task 3) and gen script `--validate` (Task 2).
3. **Wrong lld flavor / broken cross links from a non-native host** — flavor inferred from host instead of target. Expected: every `--target` produces correct magic bytes (ELF/COFF/Mach-O) even cross-host. Pinned by `hermetic_cross_*` magic-byte tests (Task 6).
4. **Bundled-lld extraction race or corruption** — two concurrent first runs, or a half-written cache file. Expected: exactly one extraction wins; size mismatch errors with the reinstall text. Pinned by extraction unit tests (Task 7).
5. **LLVM link-mode/version skew on maintainer machines** — a system dynamic LLVM satisfies `llvm-sys` and breaks at runtime, or a 23.2 prefix fails the 231 crate. Expected: build fails loudly pointing at `tools/build_toolchain.sh`. Pinned by `strict-versioning` + `prefer-static` + build-script env checks (Tasks 4–5).

## File Structure

New:
- `tools/build_toolchain.sh` — build/install LLVM+lld into `vendor/llvm-prefix`; copy standalone `lld` aside.
- `tools/gen_assets.sh` — produce per-target asset dirs (zig/xcrun), validate every target with a real lld link, copy host lld into `assets/lld/<host>/`; `--validate` mode checks committed assets without regenerating.
- `crates/cato-assets/` — `Cargo.toml`, `build.rs`, `src/lib.rs`, `assets/…` (embedded bytes).
- `crates/cato-lld-sys/` — `Cargo.toml` (feature `link`), `build.rs` (cc + llvm-config), `csrc/lld_shim.cpp`, `src/lib.rs`.
- `crates/cato-cli/src/objgen.rs` — `emit_object` via llvm-sys (`cfg(feature = "static-llvm")`).
- `crates/cato-cli/src/hermetic.rs` — capability dispatch, flag rejection, staging, argv, link (cfg-gated).
- `crates/cato-cli/tests/hermetic.rs` — feature-gated integration tests.
- `crates/cato-cli/build.rs` — `cfg(cato_bundled_lld)` emission + asset presence check.
- `.cargo/config.toml` — LLVM prefix env (Task 4).

Modified:
- `Cargo.toml` (workspace members += 2 crates), `crates/cato-cli/Cargo.toml` (features + optional deps), `crates/cato-cli/src/native.rs` (dispatch), `crates/cato-cli/src/main.rs` (mod decls), `crates/cato-cli/src/cli.rs` (`rtlib_given` flag, Task 6), `.gitignore` (`/vendor/`, macos asset dirs, `lld/`), `.github/workflows/ci.yml` (Task 8).

---

### Task 1: LLVM+lld toolchain build script

**Files:**
- Create: `tools/build_toolchain.sh`
- Modify: `.gitignore`

**Interfaces:**
- Produces: `vendor/llvm-prefix/` with `bin/llvm-config` (reports 23.1.x), `bin/lld`, `include/`, `lib/libLLVM*.a` + `lib/lld/liblld{ELF,COFF,MachO,MinGW,Common}.a` — consumed by Tasks 2, 4, 5.
- Env contract: `LLVM_SRC` (source tree, default `~/clones/llvm-project`); script refuses to run if `llvm-config` there won't report a 23.1 version (llvm-sys 231 requirement).

- [ ] **Step 1: Add `/vendor` (and build dir) to `.gitignore`**

Append:
```
/vendor
```

- [ ] **Step 2: Write `tools/build_toolchain.sh`**

`set -euo pipefail`; usage header comment documenting `LLVM_SRC` and the `vendor/llvm-prefix` output. Behavior:
1. `SRC=${LLVM_SRC:-$HOME/clones/llvm-project}`; fail with a clear message if `$SRC/llvm/CMakeLists.txt` is missing.
2. Idempotence: if `vendor/llvm-prefix/bin/llvm-config --version` starts with `23.1` and `vendor/llvm-prefix/bin/lld` exists, exit 0.
3. Version guard: run `cmake -S "$SRC/llvm" -B vendor/llvm-prefix-build` with:
   `-DCMAKE_INSTALL_PREFIX="$PWD/vendor/llvm-prefix" -DCMAKE_BUILD_TYPE=Release -DLLVM_ENABLE_PROJECTS=lld -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_TARGETS_TO_BUILD="X86;AArch6" -DBUILD_SHARED_LIBS=OFF`
   (zlib/zstd off keeps the static link surface minimal and dependency-free).
4. `cmake --build vendor/llvm-prefix-build --parallel $(sysctl -n hw.ncpu 2>/dev/null || nproc)` then `cmake --install`.
5. Final checks printed: `llvm-config --version` matches `23.1.*`, `bin/lld` exists, `lib/lld/liblldELF.a` exists (path may be `lib/liblldELF.a` on some layouts — check both, note the actual path in the script output).

- [ ] **Step 3: Run the script and verify**

Run: `tools/build_toolchain.sh` (long first build; later runs exit 0 immediately)
Then: `vendor/llvm-prefix/bin/llvm-config --version` → `23.1.*`; `vendor/llvm-prefix/bin/lld --version` → `LLD 23.1.*`.

- [ ] **Step 4: Commit**

```bash
git add tools/build_toolchain.sh .gitignore
git commit -m "feat(toolchain): build static LLVM+lld into vendor/llvm-prefix"
```

---

### Task 2: Per-target asset generation + committed assets

**Files:**
- Create: `tools/gen_assets.sh`
- Create: `crates/cato-assets/assets/x86_64-linux/*`, `crates/cato-assets/assets/aarch64-linux/*`, `crates/cato-assets/assets/x86_64-windows/*` (committed)
- Modify: `.gitignore`

**Interfaces:**
- Consumes: `vendor/llvm-prefix/bin/lld` (Task 1), `runtime/runtime.c`, `zig` (0.17+, on PATH), `xcrun` (macOS hosts only).
- Produces: the asset dirs consumed by `cato-assets` (Task 3) with exactly the required filenames from Global Constraints; `crates/cato-assets/assets/lld/<host-target-name>/lld` (gitignored) consumed by Task 7. `<host-target-name>` values: `x86_64-linux`, `aarch64-linux`, `x86_64-macos`, `aarch64-macos`, `x86_64-windows` (script derives from `uname -s`/`uname -m`).
- Script contract: `tools/gen_assets.sh` (generate + validate) and `tools/gen_assets.sh --validate` (validate committed/generated dirs only — no zig, no writes; this is what CI runs on Linux/Windows).

- [ ] **Step 1: Add generated-asset paths to `.gitignore`**

Append:
```
/crates/cato-assets/assets/x86_64-macos/
/crates/cato-assets/assets/aarch64-macos/
/crates/cato-assets/assets/lld/
```

- [ ] **Step 2: Write `tools/gen_assets.sh`**

`set -euo pipefail`; usage header documenting zig/xcrun requirements and the `--validate` mode. Behavior:

1. **Linux targets (via zig)** — for `x86_64-linux-musl` and `aarch64-linux-musl`:
   - `mkdir -p crates/cato-assets/assets/<name>` (`x86_64-linux` / `aarch64-linux`).
   - Compile the runtime: `zig cc -target <triple> -O2 -c runtime/runtime.c -o <dir>/runtime.o`.
   - Discover the rest by capturing a full static link's file list: write a throwaway `int main(void){return 0;}` C file, run `zig cc -target <triple> -static -v throwaway.c -o /tmp/throwaway 2>&1`, parse the printed command for every `.a`/`.o` path under `~/.cache/zig` (or zig's lib dir), and copy each into `<dir>/` under its basename. Required outcome: `<dir>/` ends up with `libc.a`, `crt1.o`, `crti.o`, `crtn.o`, `libcompiler_rt.a` (zig's compiler_rt archive — if zig names it differently, copy it and symlink/copy to the `libcompiler_rt.a` name). Fail loudly if any required name is missing after the copy.
2. **Windows target (via zig)** — same procedure with `-target x86_64-windows-gnu` (static link, no `-static` flag needed — mingw default); required names: `crt2.o`, `libmingw32.a`, `libmingwex.a`, `libmsvcrt.a`, `libkernel32.a` (copy any additional import libs the `-v` line names — keep them all; lld's mingw driver resolves by name).
3. **macOS targets (macOS hosts only, skipped elsewhere)** — for `x86_64-apple-darwin` / `aarch64-apple-darwin`:
   - `xcrun --sdk macosx clang -c -target <triple> -O2 runtime/runtime.c -o <dir>/runtime.o`.
   - `SDK=$(xcrun --show-sdk-path)`; copy `$SDK/usr/lib/libSystem.tbd` into `<dir>/`. Do not copy other tbds up front — the validation link below is the authority; if it demands more stubs (e.g. `libc++.tbd` is NOT expected — runtime.o is C), copy exactly what lld asks for and record it in the script's per-target tbd list.
4. **Bundled lld** — copy `vendor/llvm-prefix/bin/lld` (`.exe` suffix if host is Windows) to `crates/cato-assets/assets/lld/<host-target-name>/lld[.exe]`.
5. **Validation link per target (always, also in `--validate` mode)** — for each of the five target dirs that exists: assemble a minimal staged dir (copy the target's asset files + a trivial `main` object compiled with the same zig/xcrun command), then invoke `vendor/llvm-prefix/bin/lld` with the family argv (these are the same shapes Task 6's Rust code will build):
   - linux: `ld.lld -static -m elf_x86_64|elf_aarch64 -o out staged/crt1.o staged/crti.o prog.o staged/runtime.o -L staged -lc -lcompiler_rt staged/crtn.o`
   - windows: `ld.lld -m i386pep --subsystem console -o out prog.o staged/runtime.o -L staged`
   - macos: `ld64.lld -arch x86_64|arm64 -platform_version macos 11.0 11.0 -o out prog.o staged/runtime.o -L staged -lSystem`
   Assert the output file's magic bytes (ELF `\x7fELF` / PE `MZ` / Mach-O `0xfeedfacf` family). `--validate` runs only this half.
   Note: lld argv quirks discovered here (extra flags the link demands) must be mirrored into Task 6's `hermetic_link_argv` tests — leave a trailing comment in the script listing any deviation from the shapes above.

- [ ] **Step 3: Run generation and validation**

Run: `tools/gen_assets.sh`
Expected: all five target dirs validate; `crates/cato-assets/assets/x86_64-linux/` etc. contain the required filenames; `assets/lld/<host>/lld` exists.
Run: `tools/gen_assets.sh --validate` → exit 0 with no writes.

- [ ] **Step 4: Commit (generated dirs excepted)**

```bash
git add tools/gen_assets.sh .gitignore crates/cato-assets/assets/x86_64-linux crates/cato-assets/assets/aarch64-linux crates/cato-assets/assets/x86_64-windows
git commit -m "feat(assets): per-target runtime/CRT assets with lld validation link"
```

---

### Task 3: `cato-assets` crate

**Files:**
- Create: `crates/cato-assets/Cargo.toml`, `crates/cato-assets/build.rs`, `crates/cato-assets/src/lib.rs`
- Modify: `Cargo.toml` (workspace members += `crates/cato-assets`)

**Interfaces:**
- Consumes: asset dirs from Task 2.
- Produces (used by Tasks 6–7):
  ```rust
  pub struct TargetAssets { pub files: &'static [(&'static str, &'static [u8])] }
  pub fn assets_for(target: &str) -> Result<&'static TargetAssets, String>;
  pub fn host_target_name() -> Option<&'static str>; // cfg-derived, e.g. "aarch64-macos"
  ```
  `target` is a `driver::TARGETS` name (`"x86_64-linux"`, …, `"native"` is resolved by the caller via `host_target_name()` first; `"wasm32"` is never passed here).

- [ ] **Step 1: Write the failing tests in `src/lib.rs`**

```rust
#[test] fn linux_targets_embed_runtime_and_libc() {
    for t in ["x86_64-linux", "aarch64-linux"] {
        let a = assets_for(t).expect(t);
        let names: Vec<&str> = a.files.iter().map(|(n, _)| *n).collect();
        for req in ["runtime.o", "libc.a", "crt1.o", "crti.o", "crtn.o", "libcompiler_rt.a"] {
            assert!(names.contains(&req), "{t} missing {req}: {names:?}");
        }
        assert!(!a.files.iter().find(|(n, _)| *n == "runtime.o").unwrap().1.is_empty());
    }
}
#[test] fn windows_target_embeds_mingw_crt() {
    let a = assets_for("x86_64-windows").expect("windows");
    let names: Vec<&str> = a.files.iter().map(|(n, _)| *n).collect();
    for req in ["runtime.o", "crt2.o", "libmingw32.a", "libmingwex.a", "libmsvcrt.a", "libkernel32.a"] {
        assert!(names.contains(&req), "missing {req}: {names:?}");
    }
}
#[test] fn unknown_target_errors_with_pinned_message() {
    let e = assets_for("wasm32").expect_err("wasm has no native assets");
    assert_eq!(e, "cato: no embedded assets for target 'wasm32' (this cato build does not embed wasm32)");
}
#[test] fn host_target_name_is_known_or_none() {
    let h = host_target_name();
    if let Some(h) = h { assert!(assets_for(h).is_ok() || h.ends_with("macos")); }
}
```
(macOS hosts without generated assets are the one allowed `assets_for(h)` failure — `host_target_name_is_known_or_none` accounts for it.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-assets`
Expected: FAIL — crate does not exist yet (workspace member error).

- [ ] **Step 3: Implement the crate**

`Cargo.toml`: `name = "cato-assets"`, `version.workspace`, `edition.workspace`, no dependencies.
`build.rs`:
1. For each committed target (`x86_64-linux`, `aarch64-linux`, `x86_64-windows`): assert every required filename (Global Constraints list) exists under `assets/<t>/`; `panic!` with the missing path otherwise. Also emit `cargo:rerun-if-changed=assets`.
2. macOS: if both `assets/x86_64-macos/` and `assets/aarch64-macos/` contain their required files, emit `cargo:rustc-cfg=cato_macos_assets`; also emit `cargo:rustc-check-cfg=cfg(cato_macos_assets)` always. (macOS asset dirs are gitignored — absence is normal on Linux/Windows checkouts.)

`src/lib.rs`:
1. One `static` per target, built with `include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/<t>/<file>"))`, concatenated into a `files: &[(&str, &[u8])]` slice (write a tiny `macro_rules!` or const-array per target; no runtime indexing tricks).
2. `#[cfg(cato_macos_assets)]` equivalents for the two mac targets; without the cfg, `assets_for` on those names returns the pinned error with a suffix: `cato: no embedded assets for target 'x86_64-macos' (this cato build does not embed x86_64-macos — run tools/gen_assets.sh on macOS to generate them)`.
3. `host_target_name()` via `cfg!(target_os/target_arch)` chains returning `Some("x86_64-linux")` etc., `None` on anything else.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cato-assets` → PASS (all four).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/cato-assets
git commit -m "feat(assets): cato-assets crate embedding per-target link inputs"
```

---

### Task 4: In-process LLVM codegen (`objgen`, feature `static-llvm`)

**Files:**
- Create: `.cargo/config.toml`, `crates/cato-cli/src/objgen.rs`
- Modify: `crates/cato-cli/Cargo.toml`, `crates/cato-cli/src/main.rs`

**Interfaces:**
- Consumes: `vendor/llvm-prefix` (Task 1) via `LLVM_SYS_231_PREFIX`.
- Produces:
  ```rust
  // crates/cato-cli/src/objgen.rs — every item cfg(feature = "static-llvm")
  pub fn emit_object(ir: &str, llvm_triple: &str, opt_level: u8) -> Result<Vec<u8>, String>;
  ```
  Called by Task 6 with the hermetic triple table from Global Constraints and `cli.opt_level`.

- [ ] **Step 1: Write the failing tests at the bottom of `objgen.rs`**

`#![cfg(feature = "static-llvm")]` module tests:
```rust
const MAIN_IR: &str = "define i32 @main() {\nentry:\n  ret i32 0\n}\n";

#[test] fn emits_elf_for_musl_triples() {
    for (triple, machine) in [("x86_64-unknown-linux-musl", 0x3Eu16), ("aarch64-unknown-linux-musl", 0xB7u16)] {
        let obj = emit_object(MAIN_IR, triple, 2).unwrap();
        assert_eq!(&obj[..4], b"\x7fELF", "{triple}");
        let e_machine = u16::from_le_bytes([obj[18], obj[19]]);
        assert_eq!(e_machine, machine, "{triple}");
    }
}
#[test] fn emits_coff_for_windows_gnu() {
    let obj = emit_object(MAIN_IR, "x86_64-pc-windows-gnu", 2).unwrap();
    assert_eq!(u16::from_le_bytes([obj[0], obj[1]]), 0x8664);
}
#[test] fn emits_macho_for_darwin_triples() {
    for triple in ["x86_64-apple-darwin", "aarch64-apple-darwin"] {
        let obj = emit_object(MAIN_IR, triple, 2).unwrap();
        let magic = u32::from_le_bytes([obj[0], obj[1], obj[2], obj[3]]);
        assert!(magic == 0xfeedfacf || magic == 0xcffaedfe, "{triple}: {magic:#x}");
    }
}
#[test] fn garbage_ir_is_an_error() {
    assert!(emit_object("this is not ir", "x86_64-unknown-linux-musl", 2).is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-cli --features static-llvm objgen`
Expected: FAIL — feature/dep not declared yet.

- [ ] **Step 3: Wire features and implement `emit_object`**

`crates/cato-cli/Cargo.toml` — add under `[dependencies]`:
```toml
llvm-sys = { version = "231", optional = true, features = ["prefer-static", "strict-versioning"] }
cato-assets = { path = "../cato-assets", optional = true }
cato-lld-sys = { path = "../cato-lld-sys", optional = true, default-features = false }

[features]
static-llvm = ["dep:llvm-sys", "dep:cato-assets"]
in-process-lld = ["static-llvm", "dep:cato-lld-sys", "cato-lld-sys/link"]
```
(`cato-lld-sys` lands as an empty stub crate now so `in-process-lld` resolves; Task 5 fills it. Add `crates/cato-lld-sys` to workspace members here too, with the minimal `Cargo.toml` + empty `src/lib.rs` — Task 5 replaces the contents.)
Create `.cargo/config.toml` exactly as in Global Constraints. In `src/main.rs`: `#[cfg(feature = "static-llvm")] mod objgen;`.

`objgen.rs` implementation (no comments, minimal surface):
1. `static INIT: std::sync::Once` — on first call: `LLVM_InitializeNativeTarget()`, `LLVM_InitializeNativeAsmPrinter()`; set a fatal-error handler that panics with the message (converted to Err via `catch_unwind` is unnecessary — use `LLVMInstall fatal handler` writing to a thread-local last-error string, or rely on parse/emit returning null and build the message from the LLVM message handler set once globally with `LLVMContextSetDiagnosticHandler` per module; simplest reliable approach: check every nullable return and format a static "llvm <op> failed" string).
2. `emit_object`: create context + memory buffer (copy of `ir`), `LLVMParseIRInContext` (on failure, `llvm::write_message`-style error string via `LLVMGetErrorMessage`), `LLVMGetTargetFromTriple`, `LLVMCreateTargetMachine` (reloc `LLVMRelocDefault`, code model `LLVMCodeModelDefault`, `opt_level` mapped 0→None … 3→Aggressive), `LLVMTargetMachineEmitToMemoryBuffer` with `LLVMObjectFile`, copy bytes out, dispose everything, return `Vec<u8>`.
3. All llvm-sys calls are `unsafe`; wrap in a single safe `pub fn`.

- [ ] **Step 4: Run tests to verify they pass (slow first link)**

Run: `cargo test -p cato-cli --features static-llvm objgen`
Expected: PASS (first build compiles+statically links LLVM — many minutes; subsequent runs fast). If `llvm-sys` fails to find LLVM: confirm `vendor/llvm-prefix` exists (Task 1) and that `.cargo/config.toml` is picked up (run from the workspace root).

- [ ] **Step 5: Commit**

```bash
git add .cargo crates/cato-cli/Cargo.toml crates/cato-cli/src/main.rs crates/cato-cli/src/objgen.rs crates/cato-lld-sys Cargo.toml
git commit -m "feat(codegen): static-llvm feature emitting objects in-process via llvm-sys"
```

---

### Task 5: `cato-lld-sys` C++ shim over lld

**Files:**
- Create: `crates/cato-lld-sys/csrc/lld_shim.cpp`, `crates/cato-lld-sys/build.rs`, `crates/cato-lld-sys/src/lib.rs` (replace Task 4 stub)
- Modify: `crates/cato-lld-sys/Cargo.toml` (feature `link`)

**Interfaces:**
- Consumes: `vendor/llvm-prefix` headers + `liblld{ELF,COFF,MachO,MinGW,Common}.a` + static LLVM libs (Task 1).
- Produces (used by Task 6):
  ```rust
  #[derive(Clone, Copy, Debug)]
  pub enum Flavor { Elf, Mingw, Darwin }
  /// argv[0] is ignored by the shim (lld treats it as the program name).
  pub fn link(flavor: Flavor, argv: &[&std::ffi::CStr]) -> Result<(), String>;
  ```
  `Err(String)` carries lld's captured stderr diagnostics. Building with the `link` feature requires a C++17 compiler and `vendor/llvm-prefix`; without it the crate compiles to a no-op stub (empty `link` is never called — callers are cfg'd on `in-process-lld` which enables `link`).

- [ ] **Step 1: Write the failing Rust test in `src/lib.rs`**

```rust
#[cfg(feature = "link")]
#[test]
fn shim_reports_no_input_files() {
    let e = link(Flavor::Elf, &[]).expect_err("empty argv must fail");
    assert!(e.to_lowercase().contains("no input"), "got: {e}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cato-lld-sys --features link`
Expected: FAIL — stub `link` returns `Ok(())` / not implemented.

- [ ] **Step 3: Implement the shim**

`Cargo.toml`: `cato-lld-sys` with `features = { link = [] }`, `default = []`; `build-dependencies.cc = "1"` (optional, gated on the feature by checking `CARGO_FEATURE_LINK` in build.rs).
`csrc/lld_shim.cpp`:
```cpp
#include "lld/Common/Driver.h"
#include "llvm/Support/raw_ostream.h"
#include <cstdlib>
#include <string>

extern "C" int cato_lld_link(int flavor, const char* const* argv, int argc, char** out_err) {
  std::vector<const char*> args(argv, argv + argc);
  std::string diag;
  llvm::raw_string_ostream os(diag);
  bool ok;
  switch (flavor) {
    case 0: ok = lld::elf::link(args, os, os, /*exitEarly=*/false, /*disableOutput=*/false); break;
    case 1: ok = lld::mingw::link(args, os, os, false, false); break;
    case 2: ok = lld::macho::link(args, os, os, false, false); break;
    default: *out_err = strdup("cato-lld-sys: unknown flavor"); return 1;
  }
  os.flush();
  if (ok) { *out_err = nullptr; return 0; }
  *out_err = strdup(diag.empty() ? "lld failed" : diag.c_str());
  return 1;
}
extern "C" void cato_lld_free_err(char* p) { free(p); }
```
`build.rs` (runs only when `CARGO_FEATURE_LINK` set): locate prefix from `LLVM_SYS_231_PREFIX` (error with "run tools/build_toolchain.sh" if unset/missing); `cc::Build` compile `csrc/lld_shim.cpp` with `-std=c++17 -fno-exceptions -fno-rtti`, include `<prefix>/include` and `<prefix>/include/lld` as needed; emit `cargo:rustc-link-search=native=<prefix>/lib` plus `cargo:rustc-link-lib=static=` for `lldELF lldCOFF lldMachO lldMinGW lldCommon`; shell out to `<prefix>/bin/llvm-config --link-static --libs --system-libs` and emit its output's libs verbatim (split on whitespace, `rustc-link-lib` per token, stripping `-l`); link the C++ stdlib (`libc++` on macOS, `stdc++` on Linux, on Windows rely on the toolchain default). Also `cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX`.
`src/lib.rs`: declare the two extern fns; `link()` builds the CStr argv array, calls `cato_lld_link` with flavor discriminant, frees via `cato_lld_free_err`, maps to `Result`. Without `link`: `link()` is `unreachable!("cato-lld-sys built without the link feature")` (Task 6 never calls it — cfg'd out).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p cato-lld-sys --features link`
Expected: PASS (`no input` diagnostic). Also run `cargo test -p cato-lld-sys` (no features) → compiles, stub test absent.

- [ ] **Step 5: Commit**

```bash
git add crates/cato-lld-sys
git commit -m "feat(lld): cato-lld-sys C++ shim exposing lld::elf/mingw/macho::link"
```

---

### Task 6: Hermetic dispatch — staging, argv, in-process link

**Files:**
- Create: `crates/cato-cli/src/hermetic.rs`, `crates/cato-cli/tests/hermetic.rs`
- Modify: `crates/cato-cli/src/native.rs`, `crates/cato-cli/src/main.rs`, `crates/cato-cli/src/cli.rs`

**Interfaces:**
- Consumes: `objgen::emit_object` (Task 4), `cato_assets::{assets_for, host_target_name}` (Task 3), `cato_lld_sys::{link, Flavor}` (Task 5), `driver::TARGETS`/`LinkOpts`.
- Produces: `pub fn build_hermetic(program: &Program, cli: &Cli, out: &Path) -> Result<PathBuf, String>` in `hermetic.rs` (whole module `cfg(feature = "static-llvm")`; the in-process link arm additionally `cfg(feature = "in-process-lld")`). `native.rs::build_native` calls it for `Flavor::Native` when the feature is on; wasm and no-feature builds are untouched. Task 7 adds the `static-llvm`-only link arm.

- [ ] **Step 1: Write the failing unit tests (argv + rejections) in `hermetic.rs`**

```rust
#[test] fn rejects_cc_runtime_asan_rtlib_passthrough() {
    // build a Cli::default() per case, set the field, call the private
    // reject_unsupported via super:: (same-module unit tests), assert each
    // pinned error string.
}
#[test] fn linux_argv_shape() {
    // hermetic_link_argv("x86_64-linux", Path::new("/s"), Path::new("out"))
    // == (Flavor::Elf, ["ld.lld", "-static", "-m", "elf_x86_64", "-o", "out",
    //     "/s/crt1.o", "/s/crti.o", "/s/prog.o", "/s/runtime.o",
    //     "-L", "/s", "-lc", "-lcompiler_rt", "/s/crtn.o"])
}
#[test] fn aarch64_linux_argv_selects_elf_aarch64() { /* -m elf_aarch64 */ }
#[test] fn windows_argv_shape() {
    // == (Flavor::Mingw, ["ld.lld", "-m", "i386pep", "--subsystem", "console",
    //     "-o", "out", "/s/prog.o", "/s/runtime.o", "-L", "/s"])
}
#[test] fn macos_argv_shape_x86_64() {
    // == (Flavor::Darwin, ["ld64.lld", "-arch", "x86_64",
    //     "-platform_version", "macos", "11.0", "11.0",
    //     "-o", "out", "/s/prog.o", "/s/runtime.o", "-L", "/s", "-lSystem"])
}
#[test] fn macos_argv_shape_aarch64_uses_arm64() { /* -arch arm64 */ }
#[test] fn unknown_asset_target_errors() {
    // hermetic_link_argv("wasm32", ...) errors with the pinned no-assets string
}
```
(The exact argv is the contract — if the Task 2 validation link required extra flags, they belong in both the script comment and these assertions.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-cli --features in-process-lld hermetic`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement `hermetic.rs`**

1. `reject_unsupported(cli)`: return the pinned error for `cli.cc.is_some()`, `cli.runtime.is_some()`, `cli.asan`, `cli.rtlib_given` (new field, below), or `cli.passthrough` non-empty (first offending token in the message).
2. `resolve_target_asset_name(spec)`: `spec.name == "native"` → `cato_assets::host_target_name()` (error `cato: hermetic compile is not supported on this host for --target native` when `None`), else `spec.name`.
3. `hermetic_link_argv(target_name, staged_dir, out) -> Result<(cato_lld_sys::Flavor, Vec<String>), String>` — the shapes from the Step-1 tests, `m`/`-arch`/`platform` chosen by target family. Errors with the pinned no-assets string when `assets_for` fails.
4. `build_hermetic`: `reject_unsupported` → `LlvmBackend` emit (same as `build_native`, honoring `cli.emit_ll` by writing the IR file when asked) → `objgen::emit_object(&ir, llvm_triple_for(target_name), cli.opt_level)` → create staging dir via the existing `unique_temp` pattern (`native.rs` has it — move it to `hermetic.rs` or make it `pub(crate)`) → write `prog.o` → copy every file from `assets_for(target_name)` into the staged dir → build argv → link → remove staging dir (use a drop guard so errors also clean up) → return `out`.
5. In-process link arm: `#[cfg(feature = "in-process-lld")]` call `cato_lld_sys::link(flavor, cstrs)`; the `static-llvm`-without-lld arm is a `todo!`-free temporary: `Err("cato: hermetic linking unavailable in this build")` — Task 7 replaces it.
6. `native.rs::build_native`: before the legacy body, `#[cfg(feature = "static-llvm")]` + `if cli.target.flavor == Flavor::Native { return hermetic::build_hermetic(program, cli, out); }`. Wasm falls through unchanged.
7. `cli.rs`: add `pub rtlib_given: bool` (default `false`; set `true` in the `--rtlib` parse arm).

- [ ] **Step 4: Write the integration tests (`tests/hermetic.rs`)**

File-level `#![cfg(feature = "in-process-lld")]`. Reuse `examples/slice.cato` (expected stdout `2.5\n20\n`, per `tests/integration.rs`):
```rust
#[test] fn hermetic_host_compile_runs_without_cc() {
    // Command: cato compile examples/slice.cato -o <tempdir>/slice
    // .env("PATH", "").env_remove("CC").env_remove("CATO_CC").env_remove("CATO_RUNTIME")
    // must succeed; running the binary must print "2.5\n20\n";
    // file's first bytes: magic check per host (ELF on linux, Mach-O on macos, MZ on windows).
}
#[test] fn hermetic_cross_linux_is_elf() { /* --target x86_64-linux → \x7fELF; also aarch64-linux */ }
#[test] fn hermetic_cross_windows_is_pe() { /* --target x86_64-windows → out.exe starts MZ */ }
#[test] fn hermetic_cross_macos_is_macho() { /* --target x86_64-macos + aarch64-macos → Mach-O magic */ }
#[cfg(target_os = "linux")]
#[test] fn hermetic_linux_output_is_static() {
    // compile for the host linux target; `readelf -d <bin>` (via Command)
    // must contain no NEEDED entries.
}
#[cfg(target_os = "macos")]
#[test] fn hermetic_macos_output_links_only_libsystem() {
    // compile for the host macos target; `otool -L <bin>` lists exactly one
    // dependency, ending in "/usr/lib/libSystem.B.dylib".
}
#[test] fn hermetic_rejects_cc_flag() { /* --cc /bin/false → pinned --cc error, exit nonzero */ }
#[test] fn hermetic_rejects_runtime_flag() { /* --runtime runtime/runtime.c → pinned error */ }
#[test] fn hermetic_emit_ll_writes_ir_and_binary() { /* --emit-ll <path> keeps the .ll, still links */ }
```

- [ ] **Step 5: Run unit + integration tests**

Run: `cargo test -p cato-cli --features in-process-lld`
Expected: all PASS (first fully-hermetic end-to-end link; if lld rejects an argv shape, fix the argv — updating both unit assertions and the Task 2 script comment together).
Also run: `cargo test --workspace` (no features) → existing suite still green (legacy path untouched).

- [ ] **Step 6: Commit**

```bash
git add crates/cato-cli
git commit -m "feat(cli): hermetic native compile — in-process codegen + in-process lld"
```

---

### Task 7: Bundled-lld fallback (`static-llvm` without `in-process-lld`)

**Files:**
- Create: `crates/cato-cli/build.rs`
- Modify: `crates/cato-cli/src/hermetic.rs`, `crates/cato-cli/tests/hermetic.rs`

**Interfaces:**
- Consumes: `crates/cato-assets/assets/lld/<host-target-name>/lld` (Task 2), `hermetic.rs` staging + argv (Task 6).
- Produces: `cfg(cato_bundled_lld)` on `cato-cli` when `static-llvm` is on and `in-process-lld` is off; `hermetic::bundled::ensure_lld() -> Result<PathBuf, String>` and the link arm replacing Task 6's temporary `Err`. The bundled `lld` runs as a subprocess with the exact argv `hermetic_link_argv` produces.

- [ ] **Step 1: Write the failing extraction tests in `hermetic.rs` (bundled module)**

```rust
#[test] fn extraction_is_exactly_once_under_races() {
    // factor ensure logic as extract_to(dest: &Path, bytes: &[u8]) -> Result<(), String>
    // run two threads extracting different byte arrays to the same dest under a
    // lockfile; assert no Err and dest matches ONE of the two byte arrays whole.
}
#[test] fn size_mismatch_reports_reinstall() {
    // pre-create dest with wrong length; extract_to →
    // Err("cato: bundled lld is missing or corrupt (expected 5 bytes, found 3) — reinstall cato")
}
#[test] fn lock_timeout_is_an_error() {
    // hold a fresh lockfile, extract_to with a short timeout → Err mentioning the lock path
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-cli --features static-llvm bundled`
Expected: FAIL — module/cfg absent (and the `static-llvm`-only build currently fails at link time? No — Task 6 compiles; this run just has no tests to find → adjust: write tests under `#[cfg(all(test, feature = "static-llvm"))]` without `in-process-lld` requirement so they compile in the `static-llvm`-only configuration).

- [ ] **Step 3: Implement**

`crates/cato-cli/build.rs`:
1. Always emit `cargo:rustc-check-cfg=cfg(cato_bundled_lld)`.
2. `if env CARGO_FEATURE_STATIC_LLVM is set && CARGO_FEATURE_IN_PROCESS_LLD is not`: locate host triple name (same cfg chain as `cato_assets::host_target_name` — reimplement the 5-line match here, build.rs cannot call the crate), assert `../cato-assets/assets/lld/<host>/lld[.exe]` exists; `panic!` with: `hermetic build needs the bundled lld — run tools/build_toolchain.sh && tools/gen_assets.sh (missing <path>)`. Then emit `cargo:rustc-cfg=cato_bundled_lld`.

`hermetic.rs` — new `mod bundled`, all `#[cfg(cato_bundled_lld)]`:
1. `fn cache_path() -> PathBuf`: linux/mac `~/.cache/cato/lld-<CARGO_PKG_VERSION>/lld`, windows `%LOCALAPPDATA%\cato\lld-<CARGO_PKG_VERSION>\lld.exe` (`dirs`-free: home from `$HOME`/`USERPROFILE`, or `std::env::var_os("HOME")` with a pinned error if unset).
2. `fn extract_to(dest: &Path, bytes: &[u8], timeout: Duration) -> Result<(), String>`: create parent dir; lockfile `dest.with_extension("lock")` via `OpenOptions::new().create_new(true)` retry loop (sleep 50 ms, give up after `timeout` with `cato: timed out waiting for bundled lld extraction lock <path>`); once locked: if `dest` exists with `bytes.len()` size, return Ok; write `dest.with_extension("tmp")`, `fs::rename` onto dest, set `0o755` (unix), re-check size, remove lock. Poison/stale lock: if lockfile mtime older than 60 s, remove and retry once.
3. `fn ensure_lld() -> Result<PathBuf, String>`: `include_bytes!` of the assets lld path (build.rs already guaranteed existence), `extract_to(cache_path(), bytes, 10s)`; after extraction verify size — mismatch produces the pinned corrupt/reinstall error.
4. Link arm in `build_hermetic` (replacing Task 6's temporary): `#[cfg(all(feature = "static-llvm", not(feature = "in-process-lld")))]` → `let lld = bundled::ensure_lld()?;` then run `Command::new(lld).args(&argv).output()`, mapping failure exactly like `run_cc` (`{what} failed (exit {status}):\n{stderr}` with `what = "lld link"`).

- [ ] **Step 4: Write the fallback integration tests**

In `tests/hermetic.rs`, guarded per-test (file is `in-process-lld`-gated — so instead create the fallback tests in the same file under their own `#[cfg(all(feature = "static-llvm", not(feature = "in-process-lld")))]` **and** lift the file-level gate to `#![cfg(any(feature = "in-process-lld", feature = "static-llvm"))]`, gating the Task 6 tests with `#[cfg(feature = "in-process-lld")]` individually):
```rust
#[cfg(all(feature = "static-llvm", not(feature = "in-process-lld")))]
#[test] fn bundled_link_produces_running_host_binary() {
    // PATH cleared; cato compile examples/slice.cato; run output; "2.5\n20\n";
    // second run must be fast-path identical (cache hit) — no assertion on timing,
    // just success, plus assert the cache file exists afterwards.
}
```

- [ ] **Step 5: Run both feature configurations**

Run: `cargo test -p cato-cli --features static-llvm` → all bundled tests PASS (link needs only llvm-sys, not the C++ shim).
Run: `cargo test -p cato-cli --features in-process-lld` → still green.
Run: `cargo test -p cato-cli` (no features) → still green.

- [ ] **Step 6: Commit**

```bash
git add crates/cato-cli
git commit -m "feat(cli): bundled-lld subprocess fallback for static-llvm builds"
```

---

### Task 8: CI feature matrix

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `tools/build_toolchain.sh`, `tools/gen_assets.sh --validate`, the three feature configurations.
- Produces: continuous coverage of legacy (unchanged job), `in-process-lld`, and `static-llvm`-only, plus a cross-compile smoke pass.

- [ ] **Step 1: Add the hermetic jobs**

Keep the existing `test` job untouched (legacy path). Add:
1. `hermetic-inprocess`: matrix `{ os: [ubuntu-latest, macos-latest, windows-latest] }`; steps: checkout, rust toolchain, cache `vendor/llvm-prefix` keyed `llvm-<hashFiles('tools/build_toolchain.sh')>-<runner.os>` (on miss: run `tools/build_toolchain.sh`; on hit: verify `llvm-config --version` and skip), `tools/gen_assets.sh` (macos runners generate the tbd dirs; linux/windows just revalidate committed assets + refresh `assets/lld/<host>/lld` from the prefix), then `cargo test -p cato-cli --features in-process-lld`. Windows job needs a C++ toolchain for the shim — `ilammy/msvc-dev-cmd@v1` before the build, plus `choco install llvm` is NOT wanted (hermetic must not lean on it; only MSVC for compiling the shim).
2. `hermetic-bundled`: matrix `{ os: [ubuntu-latest, macos-latest] }` (windows excluded initially — extraction paths are unit-tested everywhere; adding windows later is a one-line matrix change): same toolchain/cache/gen steps, then `cargo test -p cato-cli --features static-llvm`.
3. `hermetic-cross-smoke` (ubuntu-latest, `in-process-lld`): after the toolchain+assets steps, `cargo test -p cato-cli --features in-process-lld hermetic_cross` — every non-host target's magic bytes validated from one host (Task 6 tests already do this; the dedicated job keeps it visible and fast).

- [ ] **Step 2: Verify locally what CI will run**

Run: `tools/gen_assets.sh --validate && cargo test -p cato-cli --features in-process-lld && cargo test -p cato-cli --features static-llvm`
Expected: green on the local host.

- [ ] **Step 3: Commit and push-check**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: hermetic feature matrix (in-process, bundled, cross smoke) with LLVM cache"
```
Open the CI run before merging: first hermetic jobs are slow (LLVM build on cache miss); confirm cache saves on a second run.

---

## Self-review notes (fixed during planning)

- Task 6's temporary `static-llvm`-only error is replaced by Task 7 in the same branch — never merged half-done (CI matrix lands in Task 8, after both).
- `rtlib_given` was added to `Cli` because `--rtlib` always has a default value and hermetic mode must only reject an *explicit* `--rtlib`/passthrough, not the default.
- macOS asset dirs and the bundled lld binary are intentionally not committed (Apple SDK license; binary size) — build.rs and gen script turn their absence into actionable errors, never silent legacy fallback.
- The Task 2 validation link and Task 6 argv unit tests pin the same argv shapes; any lld-required extra flag must be updated in both (noted in the script header comment contract).
