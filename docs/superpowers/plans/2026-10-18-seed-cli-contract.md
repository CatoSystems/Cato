# Seed CLI Contract: `compile`, driver, platforms, compiler-rt — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the seed CLI (`crates/cato-cli`) on the approved contract: `build` → `compile`, `run`/`--backend interp` removed, every compiler invocation composed by a single argv-building driver (Unix + Windows), `--` raw passthrough, `-rtlib=compiler-rt` by default, and explicit `--target` triples — no hardcoded compiler strings outside the driver.

**Architecture:** A new pure `driver` module owns the target table, toolchain resolution, and exact argv construction (option-key dedupe so passthrough replaces parsed defaults). The CLI parser implements the contract surface (subcommands, flags, `--`), `main` collapses to one compile path (IR emit or link), and the interpreter survives only in `repl` plus a dev-only driver bin used by test harnesses that previously used `cato run`.

**Tech Stack:** Rust (workspace, zero third-party deps — keep it that way), LLVM IR text pipeline, system clang, C runtime `runtime/runtime.c`.

**Spec:** `docs/superpowers/specs/2026-10-18-cli-contract-design.md` — the plan argues from it; executors read both.

## Global Constraints

- Exit codes: 0 success, 1 compile/link/tool failure, 2 usage error (spec §1) — every task's messages and tests honor these.
- No new third-party dependencies anywhere (the workspace is path-deps only today).
- All compiler invocations are argv arrays built in `crates/cato-cli/src/driver.rs`; literal `clang` command strings exist only in `driver.rs` (spec acceptance §7).
- Default `--rtlib` is `compiler-rt`; each invocation carries exactly one `-rtlib=`; passthrough with key `rtlib` replaces the parsed default (spec §5).
- Keep `repl`, `lsp`, and `--explain` behavior byte-identical (spec: only `compile` surface changes).
- Do not modify `docs/superpowers/specs/**` or historical plans; this plan lives at `docs/superpowers/plans/2026-10-18-seed-cli-contract.md`.
- The kernel (`selfhost/cato/cli.cato`) still speaks `build` until plan 3 — do not touch it here; harnesses that drive the kernel keep `build` in the kernel's argv.

## Review Focus

1. **`cato run` removal must not kill interpretation-based harnesses.** The interpreter calls `std::process::exit` directly (`cato-backend-interp/src/lib.rs:802`), so in-process migration would kill the test runner. Test: task 3's migration keeps `kernel_bootstrap`, `kernel_emit`, `kernel_reader`, `kernel_hir`, `selfhost`, and `parity` green by spawning the new subprocess driver bin, with stdout/status asserted exactly as before.
2. **Passthrough must reach the toolchain unmodified, including paths with spaces.** Test: task 4's fake-`--cc` script lives in a directory whose name contains a space and captures `"$@"` to a file; assertions inspect every argv element.
3. **`-rtlib=compiler-rt` exactly once, and replaced (not raced) by passthrough.** Test: task 1 unit tests pin argv; task 4's fake-cc test asserts `-rtlib=compiler-rt` present with no passthrough and absent when `-- --rtlib=libgcc` is given.
4. **Bare invocation and retired commands guide instead of confusing.** Test: task 2's `cli_ux` tests — bare file → exit 2 containing `subcommand required` and `cato compile`; `cato build` → exit 2 containing `renamed to 'compile'`; `cato run` → exit 2 containing `cato repl` (task 3).
5. **Flag-validation regressions after collapsing two modes into one.** Test: task 2 pins `--emit-ir` stops before linking (no artifact), `--emit-ir --emit-ll` → exit 2, `-o` valid without `--emit-ir` (it names the artifact).

---

### Task 1: `driver` module — target table, toolchain resolution, argv builder

**Files:**
- Create: `crates/cato-cli/src/driver.rs`
- Modify: `crates/cato-cli/src/main.rs` (add `mod driver;`)
- Modify: `crates/cato-cli/src/native.rs` (delete local `resolve_cc`/`resolve_runtime`, call `driver::`)

**Interfaces:**
- Consumes: nothing (pure module; `native.rs`'s existing resolve functions move here unchanged in behavior).
- Produces (task 4/5/6 rely on these exact names):
  - `pub enum Flavor { Native, Wasm }`
  - `pub struct TargetSpec { pub name: &'static str, pub triple: Option<&'static str>, pub exe_suffix: &'static str, pub flavor: Flavor }`
  - `pub const TARGETS: &[TargetSpec]` — task 1 entries: `native` (triple `None`, suffix `""`, `Flavor::Native`), `wasm32` (triple `Some("wasm32-unknown-unknown")`, suffix `".wasm"`, `Flavor::Wasm`)
  - `pub fn lookup(name: &str) -> Option<&'static TargetSpec>`
  - `pub fn default_out(spec: &TargetSpec, stem: &str) -> String` (suffix appended)
  - `pub fn resolve_cc(flag: Option<&str>) -> String` — `--cc` > `CATO_CC` > `CC` > `"clang"`
  - `pub fn resolve_runtime() -> Result<PathBuf, String>` — moved verbatim from `native.rs`
  - `pub struct LinkOpts { pub opt_level: u8, pub debug: bool, pub asan: bool, pub rtlib: String, pub passthrough: Vec<String> }`
  - `pub struct Invocation { pub program: String, pub args: Vec<String>, pub what: &'static str }` (`what` = `"link"` / `"wasm runtime"` / `"wasm link"` for error messages)
  - `pub fn link_invocations(spec: &TargetSpec, cc: &str, ll: &Path, runtime: &Path, out: &Path, opts: &LinkOpts) -> Vec<Invocation>`

- [ ] **Step 1: Write failing unit tests in `driver.rs` (`#[cfg(test)] mod tests`)**

```rust
#[test]
fn native_link_argv_baseline() {
    let spec = lookup("native").unwrap();
    let opts = LinkOpts { opt_level: 2, debug: false, asan: false,
                          rtlib: "compiler-rt".into(), passthrough: vec![] };
    let v = link_invocations(spec, "clang", Path::new("a.ll"),
                             Path::new("runtime/runtime.c"), Path::new("a"), &opts);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].args, ["-O2", "-rtlib=compiler-rt", "a.ll",
                           "runtime/runtime.c", "-o", "a"]);
}

#[test]
fn passthrough_rtlib_replaces_parsed_default() { /* --rtlib=libgcc in passthrough:
    argv contains "--rtlib=libgcc", NOT "-rtlib=compiler-rt" */ }

#[test]
fn passthrough_opt_level_replaces_parsed() { /* passthrough ["-O3"], opt_level 2:
    argv contains "-O3", NOT "-O2" */ }

#[test]
fn debug_and_asan_flags_emit_g_and_sanitize() { /* debug:true, asan:true,
    argv has "-g" and "-fsanitize=address" */ }

#[test]
fn wasm_produces_two_invocations() {
    // exact argv, task 1: runtime step args ==
    // ["--target=wasm32-unknown-unknown", "-c", "-O2", "-ffreestanding",
    //  "runtime/runtime.c", "-o", <tmp>.o]  (opt_level now, not hardcoded -O2:
    //  use opts.opt_level — see Step 3 note)
    // link step args == ["--target=wasm32-unknown-unknown", "-O{n}",
    //  "-nostdlib", "a.ll", <tmp>.o, "-Wl,--no-entry", "-Wl,--allow-undefined",
    //  "-Wl,--export=main", "-o", "out"]
}

#[test]
fn default_out_appends_target_suffix() { /* wasm32 -> "foo.wasm"; native -> "foo" */ }

#[test]
fn resolve_cc_flag_beats_env() { assert_eq!(resolve_cc(Some("mycc")), "mycc") }
```

Also port `resolve_runtime`'s existing error-path behavior with a test that sets `CATO_RUNTIME` to a missing path and expects `Err` containing `does not exist` (set/restore env inside the test).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-cli driver`
Expected: FAIL (module not found)

- [ ] **Step 3: Implement `driver.rs`**

Key decisions the tests do not pin: (a) passthrough key normalization — strip leading `-`s, split at `=`, map head matching `O[0-3]` to key `"O"`; heads otherwise are keys (`g`, `fsanitize`, `rtlib`, `target`); parsed options with a matching passthrough key are skipped, remaining parsed options keep spec order `[-O, -g, -fsanitize, -rtlib, --target=]`, then all passthrough tokens append in order, then driver-owned inputs `[… ll, runtime, -o, out]` come last (`-o` and inputs are never overridable — documented in help). (b) wasm runtime-object step uses `opts.opt_level`, not a literal `-O2` (no hardcoded values). (c) `wasm` step's temp `.o` path: derive `out` + `".runtime.o"` — no temp-collision logic needed because the file is deleted after the link (behavior ported from `native.rs:190-220`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cato-cli driver`
Expected: PASS (all 7+ tests)

- [ ] **Step 5: Move `resolve_cc`/`resolve_runtime` into `driver.rs`, delete them from `native.rs`, update call sites**

Run: `cargo test -p cato-cli` and `cargo build --workspace`
Expected: PASS — behavior unchanged (this step is a pure move).

- [ ] **Step 6: Commit**

```bash
git add crates/cato-cli/src/driver.rs crates/cato-cli/src/native.rs crates/cato-cli/src/main.rs
git commit -m "feat(seed): driver module — target table, toolchain resolution, argv builder"
```

---

### Task 2: CLI surface — `compile`, subcommand required, retired `build`, contract help

**Files:**
- Modify: `crates/cato-cli/src/cli.rs` (commands, validations, help, new flags)
- Modify: `crates/cato-cli/src/main.rs` (`Mode::Build` → `Mode::Compile`; check-mode path collapses; timing line)
- Test: `crates/cato-cli/tests/cli_ux.rs`, `crates/cato-cli/tests/integration.rs`, `crates/cato-cli/tests/emit_target.rs`
- Modify: `bench/scripts/bench.py` (`"build"` → `"compile"`), `tools/kernel_gate.sh` (`"$SEED" build` → `"$SEED" compile`; kernel `k1 build` argv unchanged), `docs/ARCHITECTURE.md`, `ROADMAP.md`, `HANDOFF.md` (only `cato build` references)

**Interfaces:**
- Consumes: `driver::lookup`, `driver::TargetSpec` (Task 1) — `Cli.target` becomes `&'static driver::TargetSpec`, default `lookup("native").unwrap()`.
- Produces: `Cli` gains `pub runtime: Option<String>`, `pub debug: bool`, `pub rtlib: String` (default `"compiler-rt"`), `pub passthrough: Vec<String>`; `Mode::Check` is gone — `Mode::Compile` now means "link an artifact unless `--emit-ir`". Task 4 consumes the new fields; `run`/`RunInterp`/`interp_args` still exist until Task 3.

Decided messages (verbatim, exit 2 unless noted):
- bare input file: `cato: subcommand required — '<file>' needs a command; try 'cato compile <file>'`
- `cato build …`: `cato: 'build' was renamed to 'compile' (cato compile <file> [-o out])`
- `cato compile` with no file: `cato: compile requires an input file (.cato)`
- no args at all: keep the existing `banner()`, but its quick-start line becomes `cato compile your_program.cato`

- [ ] **Step 1: Write failing tests**

In `tests/cli_ux.rs` add:

```rust
#[test]
fn bare_file_requires_subcommand() {
    // cato <src> -> exit 2, stderr contains "subcommand required" and "cato compile"
}
#[test]
fn build_command_is_retired() {
    // cato build <src> -> exit 2, stderr contains "renamed to 'compile'"
}
#[test]
fn compile_without_file_is_usage_error() {
    // cato compile -> exit 2, stderr contains "requires an input file"
}
#[test]
fn help_documents_contract_surface() {
    // cato help -> contains "cato compile", "--rtlib", "--runtime",
    //   "--" (passthrough doc), "EXIT STATUS"; does NOT contain "cato run"
}
```

Update: `unknown_command_suggests_closest_match` input `"bild"` → `"comple"`, expect `did you mean 'compile'` (distance 2 ≤ threshold); `check_mode_summarizes_on_stderr` → run `cato compile <src> -o <target/bin>`, assert stdout empty, stderr contains `compiled` and `ms`; `unknown_flag_suggests_closest_match` → invoke as `cato compile <src> --emit-irr`.

In `tests/emit_target.rs` update: `compile_check_mode_is_silent` → `cato compile <src> -o <tmp>` (stdout empty, timing on stderr); `ir_is_opt_in_via_emit_ir` → `cato compile <src> -o <tmp>` asserts stdout stays empty; `wasm_target_emits_wasm_module_header` / `ir_is_opt_in…` invocations get the `compile` subcommand; `wasm_run_is_rejected` → leave for Task 3.

In `tests/integration.rs` update: `build_produces_running_binary`, `build_keeps_ir_with_emit_ll`, `build_honors_opt_level_and_asan_flags` → subcommand `compile` (rename tests `compile_*`); `cli_rejects_dangling_o_flag` and `cli_rejects_unknown_flag` → invoke `cato compile <src> -o` / `cato compile <src> --no-such-flag` (flag-scan errors must still fire before the missing-file/ending checks).

Add one validation test: `emit_ir_and_emit_ll_together_rejected` → `cato compile <src> --emit-ir --emit-ll x.ll` → exit 2.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-cli`
Expected: FAIL — `bare_file_requires_subcommand`, `build_command_is_retired`, `compile_without_file_is_usage_error`, `help_documents_contract_surface` fail first; old-name tests fail after their rewrite.

- [ ] **Step 3: Rewrite `cli.rs` parsing and help**

- `COMMANDS = &["compile", "run", "repl", "lsp", "help"]` (run leaves in Task 3); retired-command check: exact arg `"build"` as first token → the retirement message + exit 2. `FLAGS` gains `--runtime`, `--rtlib`, `-g`… (bare `-g` is handled before the `starts_with('-')` catch-all; keep FLAGS as `--`-prefixed names plus `"--rtlib"`).
- Positional when no command seen: record as `file` (do NOT error yet — later flag errors must still fire first, per Step 1 ordering). At end of parse: `command` still default and `file` recorded → subcommand-required message; nothing recorded → `banner()`; `command == compile` and no file → compile-requires-input.
- New flags: `-g` → `cli.debug`; `--runtime <p>` → `cli.runtime` (value required, existing error style); `--rtlib <n>` → `cli.rtlib` (value required); `--` → push all remaining args to `cli.passthrough`, stop parsing.
- Validations after scan: `--emit-ir` only with `Mode::Compile` (run modes keep their current errors until Task 3); `--emit-ir` together with `--emit-ll` → exit 2 (`--emit-ir stops before linking; --emit-ll only applies when linking`); `-o` is now always valid in `compile` (delete the old "nothing to emit" error).
- `usage_text()`: rewrite USAGE/OPTIONS to match spec §1 exactly — `compile` forms only (no `cato <file>` check line, no `run` lines), document `-g`, `--runtime`, `--rtlib` (default compiler-rt), and a line for `--` passthrough ("everything after `--` is forwarded verbatim to the compiler driver"). Keep `EXIT STATUS` block and `--explain`/`repl`/`lsp` lines.

- [ ] **Step 4: Rewrite `main.rs` mode handling**

- `Mode::Build` renamed `Mode::Compile`; the old check-only arm is deleted: `Mode::Compile` now runs the full pipeline — `if cli.emit_ir { emit IR to stdout/`-o`; return } else { build artifact; print `compiled {file} in {n}ms` on stderr }`.
- Replace `default_stem` + manual `.wasm` formatting with `driver::default_out(cli.target, stem)`.
- Timing message: `{} compiled {} in {}ms` using `ui::check_mark()` (keeps `check_mode_summarizes_on_stderr` semantics).

- [ ] **Step 5: Update harness scripts and docs (this task's consumers only)**

- `bench/scripts/bench.py`: `"build"` arg → `"compile"` (lines ~98-99).
- `tools/kernel_gate.sh`: `"$SEED" build` → `"$SEED" compile` (kernel's own `k1 build` argv untouched).
- `docs/ARCHITECTURE.md`, `ROADMAP.md`, `HANDOFF.md`: `cato build` → `cato compile`; leave `cato run` references for Task 3.

- [ ] **Step 6: Run the suite**

Run: `cargo test --workspace`
Expected: PASS (run-dependent tests still green — `run` not yet removed).

- [ ] **Step 7: Commit**

```bash
git add crates/cato-cli/src/cli.rs crates/cato-cli/src/main.rs crates/cato-cli/tests/ \
        bench/scripts/bench.py tools/kernel_gate.sh docs/ARCHITECTURE.md ROADMAP.md HANDOFF.md
git commit -m "feat(seed)!: compile subcommand, subcommand required, retired build — CLI contract §1"
```

---

### Task 3: Remove `run` and `--backend interp`; migrate interpretation harnesses

**Files:**
- Modify: `crates/cato-cli/src/cli.rs`, `crates/cato-cli/src/main.rs` (delete run surface), `crates/cato-cli/src/native.rs` (delete `run_binary`)
- Create: `crates/cato-backend-interp/src/bin/interp_run.rs` (bin `cato-interp-run`)
- Test migrations: `crates/cato-cli/tests/integration.rs`, `crates/cato-cli/tests/emit_target.rs`, `crates/cato-backend-interp/tests/{parity,kernel_bootstrap,kernel_emit,kernel_reader,kernel_hir,selfhost}.rs`
- Modify: `docs/ARCHITECTURE.md`, `ROADMAP.md`, `HANDOFF.md` (`cato run` references)

**Interfaces:**
- Consumes: `cato_core::compile_named_with`, `cato_interp::InterpBackend { program_args }` (public field), `Backend::emit(&program) -> Result<String, String>` — the driver bin replicates the deleted `RunInterp` path exactly (`main.rs:119-143` semantics: 512MB-stack thread, `catch_unwind`-style guard, print string, exit 0/1; `(exit n)` inside interpreted code still exits via the interpreter itself).
- Produces: bin target `cato-interp-run` — argv: `cato-interp-run <file.cato> [args…]`, stdout = program output, status = 0/1 (program's own `(exit n)` wins). Harnesses spawn it at `<workspace-root>/target/debug/cato-interp-run` (same `target/debug` convention existing tests use for `cato`). Task 4+ unaffected; plan 3 (kernel) unaffected.

Why a bin and not in-process: the interpreter calls `std::process::exit` (`lib.rs:802`), so any harness running programs with `(exit …)` (the kernel CLI does) must stay behind a process boundary — see Review Focus 1.

- [ ] **Step 1: Write the failing driver bin + its contract test**

Create `src/bin/interp_run.rs`: parse args (`file` = argv[1], rest = program args), read + `cato_core::compile_named_with(file, &src, true)`, on `Err` → `eprintln!("cato: error: {e}")`, exit 1; on `Ok(program)` → spawn 512MB-stack thread running `InterpBackend { program_args }.emit(&program)` under a `catch_unwind` guard (silenced hook, ported from `cli/src/ui.rs::guard`), print the returned `String` to stdout, exit 0; panic → stderr message, exit 1.

Test in `tests/selfhost.rs` (or a new `tests/driver_bin.rs` in the same crate): `spawn driver on examples/slice.cato` → stdout `"2.5\n20\n"`, status 0; `spawn on a file that calls (exit 7)` → status 7.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cato-backend-interp driver_bin`
Expected: FAIL (bin missing)

- [ ] **Step 3: Implement the bin; tests pass**

Run: `cargo test -p cato-backend-interp driver_bin`
Expected: PASS

- [ ] **Step 4: Migrate every `cato run` harness to the driver bin**

Mechanical rule: `Command::new(<cato path>).arg("run").arg(entry).args([…])` → `Command::new(<root>/target/debug/cato-interp-run).arg(entry).args([…])`. Affected — `cato-backend-interp/tests/`: `parity.rs` (2 subprocess sites; in-process `Interpreter` sites untouched), `kernel_bootstrap.rs` (2 sites; its `build` argv for the kernel CLI stays `build`), `kernel_emit.rs` (2), `kernel_reader.rs` (1), `kernel_hir.rs` (1), `selfhost.rs` (1); `cato-cli/tests/integration.rs`: `run_native_matches_interpreter` (interp side = driver bin; native side = `cato compile <src> -o <tmp>` + run binary; keep stdout-equality assertion), `run_without_native_uses_interpreter` (driver bin with `CC=/no/such/compiler` — driver spawns no toolchain, assertion preserved). Update stale `//!`/comment references to `cato run`.

- [ ] **Step 5: Delete the run surface from the CLI**

- `cli.rs`: remove `"run"` command arm and add retirement: `cato run …` → `cato: 'run' was removed — 'cato compile' produces a binary; the interpreter lives in 'cato repl'` (exit 2); remove `Mode`, `RunInterp`, `RunNative`, `interp_args`, `--native`, `--backend` (flag + `backend` field + FLAGS entry); `COMMANDS = ["compile", "repl", "lsp", "help"]`; delete `RunInterp`/wasm-run validations; `--emit-ir` is now unconditionally valid (only mode left is `Compile` — drop the mode check).
- `main.rs`: delete `Mode` usage entirely (straight-line: parse → compile → emit-ir | link); keep `repl`/`lsp` early dispatch.
- `native.rs`: delete `run_binary`.
- Rewrite `run_without_native_uses_interpreter`'s premise comment and `wasm_run_is_rejected` (delete it — with no execution subcommand there is nothing to reject; wasm builds remain valid via `cato compile --target wasm32`).
- Add cli_ux test: `run_command_is_retired` → exit 2, stderr contains `cato repl`.

- [ ] **Step 6: Run the full suite**

Run: `cargo test --workspace`
Expected: PASS — all parity/kernel suites green through the driver bin.

- [ ] **Step 7: Update docs references + commit**

`docs/ARCHITECTURE.md`, `ROADMAP.md`, `HANDOFF.md`: replace `cato run …` usage with `cato compile …` / `cato repl` per context.

```bash
git add -A
git commit -m "feat(seed)!: remove run and --backend interp; cato-interp-run dev driver for harnesses"
```

---

### Task 4: Wire the driver — `-rtlib=compiler-rt`, `-g`, `--runtime`, `--` passthrough

**Files:**
- Modify: `crates/cato-cli/src/native.rs` (`build_native` links through `driver::link_invocations`)
- Test: new `crates/cato-cli/tests/passthrough.rs`

**Interfaces:**
- Consumes: Task 1 `driver::{link_invocations, LinkOpts, resolve_cc, resolve_runtime}`; Task 2 `Cli.{runtime, debug, rtlib, passthrough, opt_level, asan, target}`.
- Produces: final argv behavior per spec §1/§5 (acceptance greps in Task 7 depend on this replacing every `Command` construction in `native.rs` except wasm capability probes).

- [ ] **Step 1: Write the failing fake-cc test** (`tests/passthrough.rs`)

```rust
// #[cfg(unix)] — the fixture is a shell script.
// Script at "target/fake cc/cc" (SPACE in the path proves argv, not shell strings):
//   #!/bin/sh
//   printf '%s\n' "$@" > "$CATO_ARGCAP"
// Must chmod +x. Test A:
//   cato compile examples/slice.cato -o target/fake_out --cc "<script>" -g -- -DEXTRA
//   -> script exits 0; captured argv in order: "-O2", "-g", "-rtlib=compiler-rt",
//      "-DEXTRA", <temp .ll path>, "runtime/runtime.c", "-o", "target/fake_out"
//      (parsed options, then passthrough, then driver-owned inputs and -o last).
// Test B: same + `-- --rtlib=libgcc` -> argv contains "--rtlib=libgcc" and NOT "-rtlib=compiler-rt".
// Test C: `--rtlib myrt` (parsed) -> argv contains "-rtlib=myrt".
// Test D: passthrough "-O3" -> argv contains "-O3", NOT "-O2".
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cato-cli --test passthrough`
Expected: FAIL (argv today has no `-rtlib`/`-g`/passthrough; `--cc` script path with space already works — `Command` is argv-based — but composition still comes from `link_native`, so B/D fail).

- [ ] **Step 3: Rewrite `native.rs` to build through the driver**

`link_native` and both wasm steps are replaced by one path: build `LinkOpts { opt_level: cli.opt_level, debug: cli.debug, asan: cli.asan, rtlib: cli.rtlib.clone(), passthrough: cli.passthrough.clone() }`, call `driver::link_invocations(spec, &cc, ll, &runtime, out, &opts)`, run each `Invocation` with `std::process::Command::new(&inv.program).args(&inv.args)` (existing `run_cc` error formatting, using `inv.what`). Runtime path: `cli.runtime` (must exist → else error like today's `CATO_RUNTIME`) > `driver::resolve_runtime()`. Keep `resolve_wasm_cc` probes (they run `clang --print-targets`; leave them in `native.rs` — they are discovery probes, not invocations; all *command construction* lives in `driver.rs`).

- [ ] **Step 4: Run to verify passing**

Run: `cargo test -p cato-cli --test passthrough && cargo test --workspace`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cato-cli/src/native.rs crates/cato-cli/tests/passthrough.rs
git commit -m "feat(seed): all toolchain invocations via driver — rtlib/-g/--runtime/-- passthrough"
```

---

### Task 5: Phase B — explicit `--target` triples

**Files:**
- Modify: `crates/cato-cli/src/driver.rs` (target rows, `cc_supports_triple` probe)
- Modify: `crates/cato-cli/src/cli.rs` (`--target` validation via `lookup`)
- Modify: `crates/cato-backend-llvm/src/llvm/mod.rs` (`Target::Cross`)
- Modify: `crates/cato-cli/src/native.rs` (probe before link; map spec → `LlvmTarget`)
- Test: `crates/cato-cli/tests/emit_target.rs`

**Interfaces:**
- Consumes: Task 1 `TargetSpec`/`lookup`/`TARGETS`; Task 4 wiring.
- Produces: `TARGETS` rows (exact values):

| name | triple | suffix | flavor |
|---|---|---|---|
| `native` | `None` | `""` | Native |
| `wasm32` | `wasm32-unknown-unknown` | `.wasm` | Wasm |
| `x86_64-linux` | `x86_64-unknown-linux-gnu` | `""` | Native |
| `aarch64-linux` | `aarch64-unknown-linux-gnu` | `""` | Native |
| `x86_64-macos` | `x86_64-apple-darwin` | `""` | Native |
| `aarch64-macos` | `aarch64-apple-darwin` | `""` | Native |
| `x86_64-windows` | `x86_64-w64-mingw32` | `.exe` | Native |

  `pub fn cc_supports_triple(cc: &str, triple: &str) -> bool` — empty-file probe: `<cc> --target=<triple> -c -x c <empty.c> -o <temp>` succeeds. `cato_llvm::Target` gains `Cross(&'static str)` (derive becomes `Clone`; `header()` returns triple-only, datalayout omitted — clang supplies it from the triple).

- [ ] **Step 1: Write failing tests**

- Unit (`driver.rs`): `target_table_matches_spec` (assert the 7 rows' triples/suffixes/flavors), `windows_default_out_has_exe_suffix`.
- Integration (`emit_target.rs`): `unknown_target_lists_known_names` — `cato compile <src> --target powerpc` → exit 2, stderr contains `powerpc` and `x86_64-linux`; `cross_target_emits_triple_in_ir` — `cato compile <src> --target aarch64-linux --emit-ir` → stdout contains `target triple = "aarch64-unknown-linux-gnu"` and does not contain `target datalayout`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cato-cli target`
Expected: FAIL

- [ ] **Step 3: Extend the table; validate `--target` in `cli.rs` via `lookup` (unknown → exit 2 listing `TARGETS` names)**

- [ ] **Step 4: Add `Target::Cross` to `cato-backend-llvm`** — `header()` becomes `Option<(&'static str, Option<&'static str>)>`; emission at `mod.rs:265-268` writes the datalayout line only when a layout exists; map in `native.rs`/`main.rs`: spec `flavor Wasm` → `Wasm32`, `Native` + `triple None` → `Native`, `Native` + `triple Some` → `Cross(triple)`.

- [ ] **Step 5: Probe in the link path** — in `native.rs`, before running invocations: if `spec.triple.is_some() && spec.flavor == Native && !driver::cc_supports_triple(&cc, triple)` → error `cato: <cc> does not support target <triple> (known: …)` exit 1 (tool failure, not usage — the name was valid; spec §2 amendment). `--emit-ir` never probes (no toolchain touched).

- [ ] **Step 6: Run to verify passing**

Run: `cargo test -p cato-cli && cargo test --workspace`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/cato-cli/src crates/cato-backend-llvm/src/llvm/mod.rs crates/cato-cli/tests/emit_target.rs
git commit -m "feat(seed): explicit --target triples (Phase B) — table + Cross codegen + clang probe"
```

---

### Task 6: Platform correctness (Windows) + CI matrix

**Files:**
- Modify: `crates/cato-cli/src/native.rs` (platform-gated wasm guidance text: Homebrew hints behind `#[cfg(unix)]`, Windows message points at llvm.org builds)
- Create: `.github/workflows/ci.yml`
- Fix: whatever the Windows runner surfaces, bounded to platform issues (paths, suffixes, toolchain discovery) — no feature work.

**Interfaces:**
- Consumes: Task 5 `default_out` (`.exe`), driver (all argv).
- Produces: CI running `cargo build --workspace` + `cargo test --workspace` on `ubuntu-latest`, `macos-latest`, `windows-latest`.

- [ ] **Step 1: Write the workflow**

```yaml
name: ci
on: [push, pull_request]
jobs:
  test:
    strategy:
      fail-fast: false
      matrix: { os: [ubuntu-latest, macos-latest, windows-latest] }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Install LLVM (Windows)
        if: runner.os == 'Windows'
        run: choco install llvm -y
      - run: cargo build --workspace
      - run: cargo test --workspace
```

- [ ] **Step 2: Note there is no pre-existing CI** — this repo has no `.github/`; the matrix is new (spec §2 "CI matrix: macOS + Linux now, windows-latest added as part of A").

- [ ] **Step 3: Run the suite locally on macOS**

Run: `cargo test --workspace`
Expected: PASS (baseline before pushing)

- [ ] **Step 4: Push a branch, read the Windows job, fix platform failures only**

Run: workflow on GitHub; fix loop until the matrix is green. Classify each failure: platform bug → fix in code; environment (e.g. missing mingw sysroot for `x86_64-windows`) → gate the specific test `#[cfg(not(windows_ci))]`… prefer fixing; only skip with a comment naming the missing toolchain.

- [ ] **Step 5: Commit**

```bash
git add .github crates/cato-cli/src
git commit -m "ci: 3-OS matrix; platform-correct wasm guidance"
```

---

### Task 7: Acceptance pass (spec §7)

**Files:** none new — verification plus doc-comment sweeps.

- [ ] **Step 1: Grep acceptance**

Run: `rg '"clang |clang -O' crates/cato-cli/src`
Expected: matches only in `driver.rs` (probes in `native.rs` reference `cc` variables, not literals). If literals remain elsewhere, move them or document them as discovery defaults in `driver.rs`.

Run: `rg 'cato build|cato run' ROADMAP.md HANDOFF.md docs/ARCHITECTURE.md examples/`
Expected: no matches (retired-command text lives in `cli.rs`, not docs).

- [ ] **Step 2: Contract spot-check (manual)**

Run: `cargo run -p cato-cli -- help`, then verify against spec §1 table line by line: commands, every flag, `--` documented, exit codes.

- [ ] **Step 3: Full verification**

Run: `cargo test --workspace && tools/kernel_gate.sh`
Expected: all tests PASS; kernel gate reports `FIXPOINT: OK` and all fixtures OK (macOS host).

- [ ] **Step 4: Commit if any sweep changes were made; otherwise record verification in the PR/commit message of the branch**

```bash
git status  # clean if nothing to commit
```
