# CLI contract: seed + kernel, compile/lsp/format, toolchain driver

Date: 2026-10-18
Goal: make the CLI in both the seed (`crates/cato-cli`) and the kernel
(`selfhost/cato/cli.cato`) user-friendly and honest about what it does:
rename `build` → `compile`, remove `run` (one execution model — the
compiler produces a binary; the interpreter lives only in the REPL), add
`lsp` to the kernel, add a CST-based `format` to the seed, integrate
LLVM compiler-rt, let the user control every compilation argument
nothing hardcoded, and support Unix-like and Windows platforms with
explicit cross-targets falling out of that work.

Status: design, approved for planning.

Approach: **spec-first shared CLI contract, independent
implementations.** One contract document (this spec) defines the
surface; the seed implements it in Rust, the kernel in Cato. No code
sharing in either direction (the kernel cannot use Rust; the trust
model forbids the kernel depending on the seed at runtime). The
toolchain driver is designed around explicit `(TargetSpec, argv)` from
day one so that target expansion (B) and LSP parity (B) are additions,
not redesigns.

## Decisions (recorded from design dialogue)

1. `build` → `compile` in both CLIs; bare `cato <file>` is a usage
   error (exit 2) suggesting `cato compile`. Subcommand required.
2. Remove `cato run`, `run --native`, and `--backend interp`. One
   execution model: `compile` produces an artifact; the interpreter's
   only home is `repl`.
3. Kernel `lsp`: milestone A = diagnostics-only (framing, JSON-RPC,
   document sync, publishDiagnostics); milestone B immediately after =
   definition + hover at parity with the seed's current behavior.
   `textDocument/formatting` in the seed LSP is a separate, ungated
   track.
4. `format`: seed-only, Rust, built on a full-fidelity CST (comments
   and whitespace retained as trivia, rustfmt-style), rendered with a
   Wadler-style pretty-printer. The AST/HIR is explicitly *not* the
   substrate — it discards comments and layout. Kernel `format` is
   deferred.
5. compiler-rt: add `-rtlib=compiler-rt` now (scope A). Making
   `runtime.c` a proper platform-aware runtime library (static/shared,
   debug/release, per-platform object sets) is a **separate
   workstream, out of scope** here.
6. Argument pass-through model C: parsed common flags for the common
   options, plus a `--` escape hatch forwarding raw args verbatim to
   the toolchain. No compiler invocation is hardcoded in either CLI.
7. Platform support sequenced: A first (Unix-like + Windows build
   correctness: toolchain discovery, argv-based spawning, `.exe`
   handling, path separators), B right after (explicit `--target`
   triples). A's design makes B a table extension.

## 1. CLI contract (shared surface)

Both CLIs expose the same command set, implemented independently:

```
cato  <command> [options] [-- passthrough...]
cato1 <command> [options] [-- passthrough...]
```

| Command        | Seed | Kernel | Behavior |
|----------------|------|--------|----------|
| `compile <file.cato>` | yes | yes | The one build path: read → compile → emit IR → link → artifact |
| `lsp`               | yes | yes | LSP over stdio |
| `format <file>...`  | yes | —   | CST formatter (kernel deferred) |
| `repl`              | yes | —   | Interpreter's only home (kernel deferred) |
| `help [command]`, `--version` | yes | yes | Per-command help |

Rules:

- Bare `cato <file>` → usage error, exit 2, message suggests
  `cato compile <file>`.
- Unknown command → usage + did-you-mean suggestion (seed:
  edit-distance; kernel: prefix match — cheap in Cato), over **live**
  commands only.
- The retired commands `build` and `run` are recognized and answered
  with explicit migration messages (exit 2): `build` → "renamed to
  `compile`"; `run` → "removed — `cato compile` produces a binary; the
  interpreter lives in `cato repl`". No silent rename.
- Commands the kernel contract knows but does not yet implement
  (`format`, `repl`) → exit 2 with "not yet available in cato1", so
  both CLIs share the contract vocabulary.

### `compile` flags (parsed, both CLIs)

| Flag | Meaning |
|------|---------|
| `-o <path>` | Output path (default: input stem; `.wasm` suffix for wasm targets) |
| `-O0`..`-O3`, `--opt-level <n>` | Optimization level |
| `-g` | Debug info |
| `--cc <path>` | Compiler driver. Fallbacks: `CATO_CC` env → `CC` env → platform default `clang` |
| `--runtime <path>` | Runtime source. Fallbacks: `CATO_RUNTIME` env → repo-discovered `runtime/runtime.c` (walk-up, separator-agnostic) |
| `--target <name>` | Phase A: `native`, `wasm32`. Phase B: full triples (same flag, richer table) |
| `--emit-ir` | Stop after IR emission (print to stdout or `-o`) |
| `--emit-ll <path>` | Keep the emitted `.ll` at `<path>` alongside the artifact |
| `--rtlib <name>` | Runtime library, default `compiler-rt`, emitted as `-rtlib=<name>` |
| `--asan`, `--fast`, `--threads <n>`, `--no-opt`, `--dump-hir` | Carried from the existing seed flag set |
| `--` | Everything after is forwarded **verbatim** to the toolchain invocation |

- Seed-only flags today (`--fast`, `--threads`, `--dump-hir`, ...)
  remain seed-only where the kernel has no equivalent; kernel rejects
  unsupported flags with exit 2 and a clear message rather than
  silently ignoring them.
- Passthrough is appended after parsed flags so user args win last;
  the driver **dedupes by option key** (e.g. a passthrough
  `--rtlib=libgcc` replaces the parsed default instead of racing it —
  clang's duplicate-flag behavior is not a contract we rely on).
  Each option is emitted exactly once.

### Exit codes (both CLIs)

- 0 — success
- 1 — compile, link, or tool failure
- 2 — usage error

### Environment (contract)

`CATO_CC`, `CC`, `CATO_RUNTIME` — resolution order as in the flag
table. Flags always beat environment.

## 2. Toolchain driver and platform model

One abstraction per implementation, mirroring each other:

```
TargetSpec { name, triple, os_family, exe_suffix, flavor }
  Phase A: native (host triple, detected), wasm32 (wasm32-unknown-unknown)
  Phase B: adds rows — x86_64-linux, aarch64-linux, aarch64-macos,
           x86_64-windows, ... same struct, richer table, no redesign;
           validation against `clang --print-targets`.
```

**Toolchain resolution:** `--cc` → `CATO_CC` → `CC` → platform default
`clang` everywhere. `cl` is deliberately not auto-selected: the IR
pipeline is clang-oriented; Windows support means LLVM/clang on
Windows.

**Command construction: argv arrays only, never shell strings.**

- Seed: `std::process::Command` with separate args, funnelled through
  one `driver` module that builds `Vec<String>` argv from
  `(TargetSpec, parsed flags, passthrough)`. All invocations (native
  link, wasm link, IR emission follow-ups) go through it.
- Kernel: replaces `system("clang -O2 " ll ...)` string concatenation
  (broken on paths with spaces, wrong on Windows) with a new runtime
  extern `cato_spawn(argv, ...) -> exit code`, implemented in
  `runtime.c` as `posix_spawn`/`fork+execvp` on Unix-like and
  `CreateProcessW` on Windows (`#ifdef _WIN32`). Returns a decoded exit
  code, so the kernel's Unix-only `exit-of` wait-status arithmetic
  disappears from the CLI path.

**Platform handling (A):** `.exe` suffix via `exe_suffix` in
TargetSpec; separator-agnostic runtime path discovery; exit-code
normalization in the driver. **CI matrix: macOS + Linux now,
`windows-latest` (with LLVM installed) added as part of A** — that is
the honest definition of "supported".

**Phase B is a table extension:** `--target x86_64-windows` etc.
resolve to clang triple flags (e.g. `--target=x86_64-w64-mingw32`)
through the driver's table. Unknown names are usage errors (exit 2)
listing the known targets; table membership is validated by a clang
probe (an empty C compile against the triple) so a toolchain missing
the triple also fails as usage, not as a late link error. (`--print-targets`
lists codegen backends, not triples — it cannot answer this question.)

## 3. Seed CST formatter (`cato format`)

New crate `crates/cato-fmt`. `cato-core`'s reader stays untouched —
the compiler parser must not risk regression for a formatting feature.

- **Lossless CST:** a new CST parser over Cato source retaining
  everything as trivia — whitespace runs, newlines, `;` comments, and
  any token the compiler lexer discards. Tree mirrors the reader's
  grammar (forms, `defn`, `deftype`, `let`, vectors/maps, ...) with
  explicit trivia slots on every node (rustfmt/rowan style). Not
  derived from AST/HIR.
- **Wadler pretty-printer:** `Doc` algebra (`text`, `group`, `nest`,
  `line`, `concat`) with the classic best-fit algorithm. A policy layer
  decides grouping (`defn` body indent, `let` bindings one-per-line
  when a group doesn't fit). Target width via `--width <n>`, default
  100.
- **CLI:** `cato format <file>... [--check] [--width <n>]`
  - default: rewrite files in place
  - `--check`: exit 1 if any file would change (CI mode), summary printed
  - `format -` reads stdin, writes stdout
- **Idempotency guarantee:** `format(format(x)) == format(x)`, enforced
  by a property test.
- **Testing:** golden fixtures (comments in hostile positions, long
  lines, nested `let`/`match`), CST roundtrip equality
  (parse → print → parse gives equal trees).

Deferred: kernel `format`; LSP `textDocument/formatting` (separate,
ungated track).

## 4. Kernel changes: CLI rewrite + LSP

### CLI rewrite (`selfhost/cato/cli.cato`)

- **`lib/args.cato`:** flag parser per the contract — flags, values,
  `--` passthrough, positionals, unknown flag → exit 2 with
  per-command help. Replaces the positional-only `build` handling
  (`[out] [runtime.c]` positionals become `-o` / `--runtime` flags).
- **`lib/driver.cato`:** TargetSpec table + argv builder + `cato_spawn`
  extern — replaces the hardcoded `system("clang -O2 ...")`.
  Passthrough argv appended last; option-key dedupe.
- **Refactor `compile-to-ir`:** today it prints errors and returns
  `""`. Split into a pure pipeline returning structured results —
  `(ok ir)` / `(err msg line col)` — with presentation left to the
  caller (CLI or LSP). Required for LSP diagnostics; makes the pipeline
  testable.
- Commands: `compile`, `lsp`, `help`; `format`/`repl` → "not yet
  available in cato1" (exit 2).

### LSP — milestone A (diagnostics)

- **Runtime externs:** blocking stdin reads with exact byte counts
  (needed for `Content-Length:` framing) — small `runtime.c` addition
  alongside `cato_spawn`.
- **`lib/json.cato`:** minimal-but-correct JSON (objects, arrays,
  strings with escapes + UTF-8, numbers, literals), parse *and* emit.
  LSP is JSON in both directions; no dependency possible.
- **`lib/lsp.cato`:** header framing loop; `initialize`,
  `initialized`, `shutdown`, `exit`; `didOpen`/`didChange`/
  `didClose`; in-memory document store; `publishDiagnostics` from the
  structured pipeline results (positions converted to LSP 0-based
  line/character).

### LSP — milestone B (right after A)

`textDocument/definition` + `textDocument/hover` matching the seed's
*current* behavior (seed scans the in-memory document for
`(defn name ...)`; the kernel matches that behavior, not an idealized
version).

### Separate, ungated tracks

- `textDocument/formatting` in the seed LSP (backed by `cato-fmt`).
- Kernel `format` and `repl`.

## 5. compiler-rt integration

- The driver (both CLIs) emits `-rtlib=compiler-rt` by default via the
  parsed `--rtlib` flag. Nothing hardcoded inline: it is a
  driver-level default like `--cc`, overridable
  (`--rtlib=libgcc`) or replaced by passthrough dedupe.
- Emission rules: each option exactly once; parsed flags first,
  passthrough last; passthrough replaces a parsed default of the same
  option key rather than duplicating it.
- If a platform's clang rejects `-rtlib=compiler-rt`, the failure
  surfaces as a normal link error carrying the underlying clang
  message. No silent fallback — explicit beats magic. No probe/caching
  (YAGNI).
- **Out of scope (separate workstream):** `runtime.c` as a proper
  platform-aware runtime library — static/shared, debug/release,
  per-platform object sets, installable artifacts.

## 6. CLI UX

- Consistent error prefixes: `cato: ...` / `cato1: ...` everywhere
  (seed's `ui::error_lead()` unified to `cato:`; kernel's mixed
  `err ` / `cato1:` outputs normalized).
- Per-command help: `cato help compile` prints that command's full
  flag list, including an explicit line documenting `--` passthrough.
- Seed keeps its timing line (`compiled X in Nms`); kernel prints
  nothing on success (bootstrap-friendly).
- Same exit codes everywhere (see §1).

## 7. Testing and acceptance

**Tests:**

- Contract tests (seed `tests/cli_ux.rs` extended): golden help/usage
  per command; exit-code matrix (0/1/2); bare-invocation error; `--`
  passthrough reaches argv (assert via a fake `--cc` script echoing
  its args); `--rtlib` passthrough-replaces-default; option dedupe.
- Driver unit tests: argv construction per TargetSpec × flag matrix;
  `cfg(windows)`/`cfg(unix)` construction-level assertions (`.exe`,
  argv shape) — no Windows host required for construction tests.
- Format: golden files, idempotency property, CST roundtrip equality.
- LSP: framed-message harness — feed `Content-Length`-framed
  `initialize`/`didOpen`/... and assert JSON responses; the same
  harness pattern reused for the kernel LSP (spawn `cato1 lsp` as a
  subprocess).
- Kernel: `tools/kernel_gate.sh` extended for the new commands;
  fixtures for `cato1 compile` flags/passthrough.
- CI: macOS + Linux + Windows (`windows-latest`, LLVM installed).

**Acceptance:**

1. Seed and kernel CLIs behave identically per contract: same
   commands, same flags, same exit codes on macOS, Linux, Windows.
2. No hardcoded compiler invocations: searching the two CLI trees for
   literal `clang ` command strings returns nothing outside the
   driver's argv tables/defaults.
3. Link argv contains `-rtlib=compiler-rt` by default; passthrough
   `--rtlib=...` replaces it (exactly one `-rtlib=` in argv).
4. `cato format` golden tests + idempotency pass; comments preserved.
5. `cato1 lsp` passes milestone-A harness (initialize/diagnostics);
   milestone B adds definition/hover responses matching seed behavior.

## 8. Sequencing

1. This spec is the source of truth for the contract; writing-plans
   derives the implementation plan from it.
2. **Seed A:** renames/removals (`compile`; drop `run`,
   `run --native`, `--backend interp`) + `driver` module + `--rtlib`
   default + Unix/Windows correctness + CI matrix.
3. **Seed B:** target-table expansion (`--target` full triples).
4. **Seed format:** `cato-fmt` crate + CLI wiring.
5. **Kernel:** runtime externs (`cato_spawn`, stdin reads) →
   `lib/args.cato` → `lib/driver.cato` → CLI rewrite (`compile`).
6. **Kernel LSP A** (json + framing + diagnostics) → **LSP B**
   (definition/hover parity).
7. Separate/ungated tracks: runtime-as-library workstream; seed LSP
   `textDocument/formatting`; kernel `format`/`repl`.
