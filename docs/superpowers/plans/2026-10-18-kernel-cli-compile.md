# Kernel CLI Rewrite: `cato1 compile`, argv spawn, contract flags — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the kernel CLI (`selfhost/cato/cli.cato`) on the shared contract: `compile` (retiring `build`), flag parsing with `--` passthrough, a Cato-side toolchain driver spawning via argv (`cato_spawn`, never shell strings), structured compile results, stderr/`cato1:` diagnostics — zero hardcoded compiler invocations.

**Architecture:** Three new runtime primitives + env/file helpers land in `runtime/runtime.c` **and** the seed interpreter's modeled-extern table (every kernel harness drives the CLI through the interpreter). On top: `lib/args.cato` (parser), `lib/driver.cato` (target table + argv builder, mirroring the seed driver's exact ordering/dedupe rules), then the `cli.cato` rewrite. The kernel's `system` extern and `exit-of` wait-status arithmetic are deleted.

**Tech Stack:** Cato (self-hosted kernel), `runtime/runtime.c` (C99), Rust test harnesses in `crates/cato-backend-interp/tests/`, `tools/kernel_gate.sh`.

**Spec:** `docs/superpowers/specs/2026-10-18-cli-contract-design.md` §1/§2/§4/§6 — the plan argues from it; executors read both. Seed driver behavior to mirror: plan 1 (`docs/superpowers/plans/2026-10-18-seed-cli-contract.md`).

## Global Constraints

- Exit codes: 0 success, 1 compile/link/tool failure, 2 usage error — `exit` from `main` only (spec §1).
- No shell strings: the only process launch is `cato_spawn` with a `(Vector str)` argv; literal `clang` appears only in `lib/driver.cato` (spec acceptance §7 grep).
- Kernel CLI flags: `-o`, `-O0..-O3`, `--opt-level`, `-g`, `--cc`, `--runtime`, `--target`, `--emit-ir`, `--emit-ll`, `--asan`, `--rtlib`, `--`, `--help`. Any other flag (including seed-only `--fast`/`--threads`/`--no-opt`/`--dump-hir`) → `cato1: unknown flag …`, exit 2 (spec §1).
- Error/usage output goes to **stderr** with the `cato1: ` prefix (spec §6); success prints nothing; `--emit-ir` prints IR to stdout.
- `cato1` accepts `format`/`repl`/`lsp` as command names but answers `not yet available in cato1` (exit 2) until plans 4 (lsp) and later — so `lsp` is stubbed here, implemented in plan 4.
- Kernel `--target` table: `native` + the explicit native triples from plan 1's table; `wasm32` → exit 2 `cato1: wasm32 target not yet supported` (kernel emitter has no wasm mode; contract §1 allows kernel-surface rejection with exit 2).
- Every new extern must be modeled in `crates/cato-backend-interp/src/lib.rs` or all kernel harnesses die on `unsupported extern`.
- Historical docs (`docs/superpowers/specs/**`, old plans) are not edited; live docs (`ROADMAP.md`, `HANDOFF.md`) update `cato1 build` → `cato1 compile` in Task 4.

## Review Focus

1. **Every new extern must work twice: natively (runtime.c) and under the interpreter harness.** Test: Task 1's `kernel_externs.rs` runs each extern fixture both via `cato-interp-run` (subprocess) and via `cato compile` + native execution, asserting identical output.
2. **Error routing changed stdout → stderr with a new prefix; every harness reading `err ` on stdout must move with it.** Test: Task 1 greps `starts_with("err` / `"err "` consumers; Task 4 asserts `kernel_bootstrap` now reads stderr, and a direct test asserts `cato1 compile <bad>` writes `cato1: ` to stderr, nothing to stdout.
3. **Argv parity with the seed driver** (same order, same dedupe keys, inputs/`-o` last). Test: Task 3's fixture prints the driver-built argv as a joined string; the assertion is the same exact sequence plan 1's `native_link_argv_baseline` pins for equivalent inputs.
4. **Exit-code matrix under the interpreter** (the harness path the gate also relies on). Test: Task 4's `kernel_cli.rs` asserts 0 (ok), 1 (compile error / spawn fail), 2 (unknown flag, unknown target, missing file arg, retired `build`).
5. **Self-host fixpoint must survive the rewrite** (k2/k3 byte-identical `.ll`). Test: Task 5 runs `tools/kernel_gate.sh` to `GATE: PASS` including `FIXPOINT: OK`.

---

### Task 1: Runtime primitives — spawn, env, file-exists, stdin, stderr, flush

**Files:**
- Modify: `runtime/runtime.c`
- Modify: `crates/cato-backend-interp/src/lib.rs` (modeled-extern table)
- Test: `crates/cato-backend-interp/tests/kernel_externs.rs`

**Interfaces:**
- Consumes: existing FFI constraints (return `i64/f64/bool/str`; params scalar/`str`/`vec` — see `examples/ffi.cato` and `funcs.rs:252-289`).
- Produces (Tasks 2–4, plan 4 rely on these exact Cato extern signatures):

```cato
(extern cato-spawn [argv :- (Vector str)] :- i64)     ; argv[0] + argv[1..]; decoded exit code; -1 spawn failure
(extern cato-getenv [name :- str] :- str)             ; value or "" if unset
(extern cato-file-exists [path :- str] :- bool)
(extern stdin-getline :- str)                         ; next line WITHOUT trailing \r\n; "" at EOF; blocking
(extern stdin-read-exact [n :- i64] :- str)           ; exactly n bytes, blocking; shorter ONLY at EOF
(extern stdout-flush :- i64)                          ; fflush(stdout); always 0
(extern cato-stderr [s :- str] :- i64)                ; fputs to stderr (unbuffered-ish); always 0
```

- C side names use underscores (`cato_spawn`, …); the emitters map dashes (existing convention: `cato-stab-new` ↔ runtime symbol). The **`(Vector str)` ABI for `cato_spawn` must be read off the seed's FFI lowering** (`crates/cato-backend-llvm/src/llvm/funcs.rs:252-289`) and `runtime.c`'s existing vec helpers — the end-to-end test in this task pins it; do not guess in isolation.
- Interp modeling: `cato-spawn` → `std::process::Command::new(argv[0]).args(&argv[1..]).status()` → `code.unwrap_or(1)`, spawn error → `-1`; `cato-getenv` → `std::env::var` (miss → `""`); `cato-file-exists`; `stdin-*` → `std::io::stdin()` locked reads (line trim of `\r\n`; exact = loop until n or EOF); `stdout-flush` → no-op `0` (interp captures output in memory); `cato-stderr` → `eprint!`.

- [ ] **Step 1: Write failing tests (`kernel_externs.rs`)**

Fixtures are small Cato programs written by the test into `target/extern_tests/`, each with `main`, run two ways: `Command::new(root/target/debug/cato-interp-run).arg(fixture)` and `cato compile fixture -o bin` + run bin (the native half exercises `runtime.c`).

```rust
#[test] fn spawn_returns_decoded_exit_code()     // main prints (cato-spawn ["sh" "-c" "exit 7"]) -> "7"; #[cfg(unix)]
#[test] fn spawn_missing_program_is_minus_one()   // ["definitely-not-a-program"] -> "-1"
#[test] fn getenv_reads_environment()             // set CATO_T_ENV in Command env -> prints value; unset -> prints "" (print-str "" + newline)
#[test] fn file_exists_true_and_false()           // -> "true" / "false" (bool prints)
#[test] fn stdin_read_exact_takes_piped_bytes()   // spawn with stdin piped "ABCD", fixture reads (stdin-read-exact 4) and prints -> "ABCD"
#[test] fn stdin_getline_strips_newline_and_hits_eof() // piped "x\r\ny" -> getline "x", getline "y", getline "" -> prints "x|y|"
#[test] fn stderr_extern_writes_to_stderr()       // (cato-stderr "boom\n") -> child stderr contains "boom", stdout does not
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cato-backend-interp kernel_externs`
Expected: FAIL (`unsupported extern` / spawn errors)

- [ ] **Step 3: Implement `runtime.c` functions** — `cato_spawn`: heap-build a `char**` of length n+1 (NUL-terminated) from the Cato vector ABI, `fork`/`execvp` + `waitpid` on Unix (`#ifdef _WIN32`: `CreateProcessW` + `GetExitCodeProcess`, wide-char conversion — this is the plan-1-spec'd platform surface), decode to exit code; return `-1` on failure. The rest are thin libc wrappers (`getenv`, `access`/`fopen`, `fgets`-loop, `fflush`, `fputs(stderr)`).

- [ ] **Step 4: Model the externs in `crates/cato-backend-interp/src/lib.rs`** (beside the existing `system` arm ~line 201) — see Interfaces for exact semantics.

- [ ] **Step 5: Run to verify passing — both halves**

Run: `cargo test -p cato-backend-interp kernel_externs`
Expected: PASS — native (`runtime.c`) and interp halves both asserted per test.

- [ ] **Step 6: Commit**

```bash
git add runtime/runtime.c crates/cato-backend-interp crates/cato-backend-interp/tests/kernel_externs.rs
git commit -m "feat(kernel): runtime primitives — cato-spawn argv spawn, env/file/stdin/stderr externs"
```

---

### Task 2: `lib/args.cato` — contract flag parser

**Files:**
- Create: `selfhost/cato/lib/args.cato`
- Test: `crates/cato-backend-interp/tests/kernel_cli.rs` (created here, extended in Tasks 3–4)

**Interfaces:**
- Consumes: Task 1 extern `cato-getenv` (for nothing yet — env resolution is the driver's); kernel stdlib (`subs`, `count`, `=`, `str`, `(get v i default)` on `(Vector str)`, `parse-i64`, `str <`/`=` comparisons).
- Produces (cli.cato in Task 4 relies on this exact API):

```cato
(ns cato.args)
(deftype ArgRes
  (ArgOk [file :- str out :- str cc :- str runtime :- str target :- str
          opt :- i64 debug :- i64 asan :- i64 emit-ir :- i64
          emit-ll :- str rtlib :- str help :- i64 rest :- (Vector str)])
  (ArgErr [msg :- str]))
; (parse (args)) — argv INCLUDING argv[0] (the program name), contract §1 rules:
;  - command word argv[1] must be "compile" (others handled by cli.cato before parse)
;  - defaults: out "" (= driver derives from file stem), cc/runtime "" (env resolved later),
;    target "native", opt 2, debug/asan/emit-ir/help 0, emit-ll "", rtlib "compiler-rt"
;  - first non-flag token after the command = file (second positional → ArgErr)
;  - "--" ends parsing: all remaining tokens → rest verbatim
;  - missing values, bad -O level, unknown flags → ArgErr with the message text
(defn parse [a :- (Vector str)] :- ArgRes …)
```

Parsing rules to pin: `-O0`..`-O3` and `--opt-level N` (N 0–3); `--emit-ir`/`-g`/`--asan`/`--help` are booleans; `-o/--cc/--runtime/--target/--emit-ll/--rtlib/--opt-level` take the next token (missing → `ArgErr "-o requires a filename"`-style text matching the seed's messages); `--` starts rest.

- [ ] **Step 1: Write failing tests (`kernel_cli.rs`, part 1 — parser only)**

Harness pattern (copy `modules.rs` scaffolding): write a driver source under `target/kernel_cli/parse_main.cato` with `(require ["../../selfhost/cato/lib/args.cato" :as args])` and a `main` that pattern-matches `(args/parse (args))` and prints a canonical single line per outcome, e.g. ArgErr → `E <msg>`; ArgOk → `F file out target opt debug asan emit-ir rtlib rest=…` (fields joined with `|`). Then `Command` spawn `cato-interp-run parse_main.cato -- <argv…>` with argv provided as the fixture's program args, assert exact stdout.

```rust
#[test] fn parses_minimal_compile()        // argv: compile f.cato -> F f.cato||native|2|0|0|0|compiler-rt|rest=
#[test] fn parses_full_flag_set()          // -o out -O3 -g --asan --target native --rtlib myrt --emit-ir -- cc --runtime r.cato f.cato
#[test] fn dash_dash_rest_is_verbatim()    // compile f.cato -- -O3 -v -> rest contains "-O3|-v"
#[test] fn unknown_flag_is_argerr()        // --bogus -> E unknown flag --bogus
#[test] fn missing_value_is_argerr()       // -o (nothing after) -> E …requires…
#[test] fn second_positional_is_argerr()   // compile a.cato b.cato -> E
#[test] fn bad_opt_level_is_argerr()       // --opt-level 9 -> E
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-backend-interp kernel_cli` → FAIL

- [ ] **Step 3: Implement `lib/args.cato`** — single forward loop over `(args)` with an index (recursion or `loop`/`recur` per kernel style in `reader/read.cato`); build `rest` via `conj`; field accumulation via `let`.

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-backend-interp kernel_cli` → PASS

- [ ] **Step 5: Commit**

```bash
git add selfhost/cato/lib/args.cato crates/cato-backend-interp/tests/kernel_cli.rs
git commit -m "feat(kernel): lib/args.cato — contract flag parser with -- passthrough"
```

---

### Task 3: `lib/driver.cato` — target table, argv builder, spawn

**Files:**
- Create: `selfhost/cato/lib/driver.cato`
- Modify: `crates/cato-backend-interp/tests/kernel_cli.rs` (extend)

**Interfaces:**
- Consumes: Task 1 externs (`cato-spawn`, `cato-getenv`), Task 2's `ArgRes` fields.
- Produces (cli.cato Task 4 relies on):

```cato
(ns cato.driver)
; Target table: parallel (Vector str) — names, triples ("" = host default, no --target flag),
; suffix ("" or ".wasm"/".exe"). Rows mirror plan 1's table exactly, minus wasm32:
;   native | x86_64-linux | aarch64-linux | x86_64-macos | aarch64-macos | x86_64-windows
(defn target-index [name :- str] :- i64)        ; -1 = unknown
(defn target-triple [i :- i64] :- str)
(defn target-suffix [i :- i64] :- str)
(defn default-out [i :- i64 file :- str] :- str) ; stem-of file + suffix  (stem-of lives in cli.cato today — MOVE here)
(defn resolve-cc [flag :- str] :- str)           ; flag > CATO_CC > CC > "clang"   (cato-getenv)
(defn resolve-runtime [flag :- str] :- str)      ; flag > CATO_RUNTIME > "runtime/runtime.c"
(defn link-argv [target-i :- i64 opt :- i64 debug :- i64 asan :- i64
                 rtlib :- str rest :- (Vector str)
                 cc :- str ll :- str rt :- str out :- str] :- (Vector str))
(defn spawn [argv :- (Vector str)] :- i64)       ; cato-spawn wrapper
```

`link-argv` must reproduce plan 1's ordering and dedupe **byte-for-byte**: parsed `[-O, -g, -fsanitize, -rtlib, --target=]` (skip any whose key matches a passthrough token — key normalization: strip leading `-`s, split at `=`, `O[0-3]` → `"O"`), then passthrough tokens, then inputs `[ll rt -o out]`. `--rtlib` default `compiler-rt` is applied by the CLI (Task 4) before calling; `link-argv` only dedupes.

- [ ] **Step 1: Write failing tests (extend `kernel_cli.rs`)**

Fixture `target/kernel_cli/drv_main.cato` requires `lib/driver`, `main` = `(print-str (str/join (driver/link-argv …fixed scalars… (args) "cc" "a.ll" "rt.c" "out") " "))` — passthrough comes from the fixture's own program args (no literal `(Vector str)` construction needed; vector literals are i64/f64-only).

```rust
#[test] fn link_argv_baseline_matches_seed()    // no extra args -> "-O2 -rtlib=compiler-rt a.ll rt.c -o out"
#[test] fn passthrough_rtlib_replaces_default()  // args ["--rtlib=libgcc"] -> contains --rtlib=libgcc, no -rtlib=compiler-rt
#[test] fn passthrough_dash_o3_replaces()        // ["-O3"] with opt 2 -> "-O3", no "-O2"
#[test] fn windows_row_gets_exe_and_triple()     // target-index "x86_64-windows" = 5? (assert by lookup, not index literal) -> default-out → "f.exe"; link-argv contains --target=x86_64-w64-mingw32
#[test] fn resolve_cc_precedence()               // flag wins; env via Command env CATO_CC/CC; default "clang"
#[test] fn spawn_wrapper_returns_code()          // (driver/spawn ["sh" "-c" "exit 3"]) -> prints "3"
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-backend-interp kernel_cli` → FAIL

- [ ] **Step 3: Implement `lib/driver.cato`** — tables as three parallel `(Vector str)` built with `conj`; string scan helpers (`subs`/`count`/`nth`); dedupe loop over `rest`.

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-backend-interp kernel_cli` → PASS (argv strings match plan 1's baselines exactly)

- [ ] **Step 5: Commit**

```bash
git add selfhost/cato/lib/driver.cato crates/cato-backend-interp/tests/kernel_cli.rs
git commit -m "feat(kernel): lib/driver.cato — target table + argv builder mirroring the seed driver"
```

---

### Task 4: `cli.cato` rewrite — `compile`, structured pipeline, `cato1:` stderr

**Files:**
- Create: `selfhost/cato/lib/compile.cato` (the structured pipeline — lives in `lib/` from the start so plan 4's `lsp.cato` can require it without a cli↔lsp require cycle)
- Modify: `selfhost/cato/cli.cato`
- Modify: `crates/cato-backend-interp/tests/kernel_bootstrap.rs` (argv + stderr migration)
- Modify: `tools/kernel_gate.sh` (flags), `ROADMAP.md`, `HANDOFF.md` (`cato1 build` → `cato1 compile`)
- Test: extend `crates/cato-backend-interp/tests/kernel_cli.rs`

**Interfaces:**
- Consumes: Tasks 1–3 (all of `lib/args`, `lib/driver`, externs).
- Produces:
  - **Structured pipeline** (plan 4's LSP depends on this exact API): replace today's `compile-to-ir` (prints `err …`, returns `""`) with

    ```cato
    (deftype CRes (CROk [ir :- str]) (CRErr [msg :- str line :- i64 col :- i64 len :- i64]))
    ; line 1-based, col 0-based (reader Span convention), len = span length;
    ; line 0 = "no position" (HIR-lower/moveck errors carry no span today — documented
    ; fallback; reader/loader/expand errors carry real spans)
    (defn compile-text [path :- str src :- str] :- CRes ...)  ; PURE: no printing, no IO
    ```
    Defined in `selfhost/cato/lib/compile.cato` (ns `cato.compile`). `cli.cato` slurps, requires it, calls `compile-text`, and formats; the LSP (plan 4) requires the same lib and passes stored document text directly.
  - Commands: `compile` (real), `help` (contract usage text), `build` → `cato1: 'build' was renamed to 'compile' (cato1 compile <file> [-o out])` exit 2, `format`/`repl`/`lsp` → `cato1: <cmd> is not yet available in cato1` exit 2 (lsp flips in plan 4), unknown/missing → usage + exit 2 (prefix-match did-you-mean over `compile help format repl lsp build` where a candidate starts with the input).
  - `compile` flow: parse → `ArgErr` → `cato1: <msg>` stderr exit 2; `--help` → usage stdout exit 0; file missing (`cato-file-exists`) → `cato1: no such file <f>` exit 2; unknown target → `cato1: unknown target '<t>' (known: …)` exit 2; compile-text `CRErr` → stderr `cato1: <msg> [@line:col]` exit 1 (keep the `@line:col+len` fragment so position-aware harnesses keep working); success: `--emit-ir` → `print-str ir` exit 0; else `spit (str out ".ll")` → `driver/link-argv` → `cato-spawn`; nonzero → `cato1: cc failed (exit N)` stderr exit 1; else exit 0, **no stdout output**.
  - Delete: `(extern system …)`, `exit-of`, the old positional `[out] [runtime.c]` handling, old `usage`.

- [ ] **Step 1: Write failing tests (extend `kernel_cli.rs`)**

```rust
#[test] fn compile_builds_a_fixture_binary()     // cato-interp-run cli.cato compile <fixture> -o <tmp> -> exit 0, binary exists and runs
#[test] fn compile_error_goes_to_stderr_with_prefix() // bad fixture -> stdout empty, stderr starts "cato1: ", exit 1
#[test] fn build_is_retired()                    // argv build f.cato -> exit 2, stderr contains "renamed to 'compile'"
#[test] fn unknown_flag_exits_two()              // --bogus -> exit 2, stderr "cato1: unknown flag"
#[test] fn unknown_target_exits_two_and_lists()  // --target powerpc -> exit 2, stderr lists "native" and "x86_64-linux"
#[test] fn missing_input_file_exits_two()        // compile (no file) -> exit 2 "requires an input file"
#[test] fn emit_ir_prints_ir_to_stdout()         // --emit-ir -> stdout contains "define i32 @main" (or fixture-appropriate), exit 0
#[test] fn help_documents_contract_flags()       // help -> stdout contains -o, --rtlib, --runtime, --, EXIT STATUS-ish exit-code doc; exit 0
```

Update `kernel_bootstrap.rs`: argv `["build", file, out, rt]` → `["compile", file, "-o", out]` (runtime default resolves via cwd = repo root); its out-of-contract check `build_out.starts_with("err ")` → stderr `String::from_utf8_lossy(&build.stderr).starts_with("cato1:")`.

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-backend-interp kernel_cli kernel_bootstrap` → FAIL

- [ ] **Step 3: Move the pipeline into `lib/compile.cato` (`compile-text`/`CRes`)** — thread spans from `RErr/EErr/ORErr` (already carry `Span`); lower/moveck errors → `line 0`. No printing anywhere in the pipeline (its callers format).

- [ ] **Step 4: Rewrite `main`/`compile-cmd`/`help` per Interfaces; delete `system`+`exit-of`** — construction/match idioms: copy `(RErr msg span)` positional style from `reader/read.cato:260`.

- [ ] **Step 5: Update gate, bootstrap, docs**

- `tools/kernel_gate.sh`: seed step already `compile` (plan 1); `kb()` → `/tmp/k1 compile "$1" -o "$2"`; the two self-build lines → `compile selfhost/cato/cli.cato -o /tmp/k2` (and k3); `check_bad` substring expectations re-read against the new stderr text.
- `ROADMAP.md`/`HANDOFF.md`: `cato1 build` → `cato1 compile` (leave historical spec/plan docs alone).
- `rg '"err "'` across `crates/ selfhost/ tools/` → fix remaining consumers.

- [ ] **Step 6: Run everything**

Run: `cargo test --workspace && tools/kernel_gate.sh`
Expected: all tests PASS; gate `GATE: PASS` with `FIXPOINT: OK`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(kernel)!: cato1 compile per contract — structured pipeline, argv driver, cato1: stderr"
```

---

### Task 5: Acceptance pass (spec §7 kernel half)

- [ ] **Step 1: Grep acceptance**

Run: `rg '\(extern system|exit-of|system \(' selfhost/`
Expected: no matches in kernel CLI/driver paths (fixtures may keep unrelated uses — none are expected).

Run: `rg '"clang' selfhost/cato/`
Expected: only `lib/driver.cato`.

- [ ] **Step 2: Contract spot-check (manual, interp-driven)**

Run: `cargo build --workspace`, then for `f in help "compile f.cato --bad" "build f.cato"`: `target/debug/cato-interp-run selfhost/cato/cli.cato $f` — verify exit codes 0/2/2 and messages against spec §1.

- [ ] **Step 3: Full verification**

Run: `cargo test --workspace && tools/kernel_gate.sh`
Expected: PASS + `GATE: PASS`, `FIXPOINT: OK`.
