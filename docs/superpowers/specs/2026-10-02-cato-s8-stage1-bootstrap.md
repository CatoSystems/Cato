# S8 — Stage 1: the kernel builds itself (`selfhost/cato/cli.cato`)

Date: 2026-10-02
Goal: the kernel gets a CLI and the bootstrap chain stands up — seed
builds cato1, cato1 builds cato2, cato2-built binaries run the reference
programs in parity with `cato run`. Acceptance per the ROADMAP: a kernel
CLI skeleton (read source → compile → write `.ll` → spawn `cc` → native
binary, with real exit codes), and the cato1 → cato2 link closing.

Status at the end of this phase: **the CLI skeleton and the bootstrap
harness are complete and green through the seed interpreter; the
cato1 → cato2 leg is built and blocked by one remaining seed-emitter
use-after-free on the full kernel** (details below). Everything
else the phase needed — including six seed compiler bugs that stood
between the seed's LLVM backend and the kernel's own source — is fixed
and guarded.

## The kernel CLI (`selfhost/cato/cli.cato`)

The kernel entry: it `require`s the lib, reader, hir and emit modules
(merged and canonicalized by the loader in `hir/modules.cato`) and
defines the only `main`. The skeleton:

```
cato1 build <file.cato> [out] [runtime.c]
```

read → expand → build → moveck → emit (the S6/S7 pipeline, unchanged) →
`spit <out>.ll` → `system`("clang -O2 <out>.ll <rt> -o <out>"). Exit
codes are real: 0 success, 1 compile error or cc failure, 2 usage. The
output path defaults to the input's stem, the runtime to
`runtime/runtime.c` relative to the working directory; both positional
(no flag parsing in the skeleton). The C compiler runs through the
`system` extern — the wait status decodes with an arithmetic shift
(`(/ status 256)`, `-1` guarded), and the seed interpreter models
`system` with the same convention (crates/cato-backend-interp), so the
driver behaves identically under `cato run` and, once built, natively.

## Module layout (`selfhost/cato/cli.cato`)

The kernel source is `require`-connected modules: `cli.cato` is the
entry, and each file carries `(ns cato.<path>)` plus `(require
["<relative>" :as <alias>])` forms. `hir/modules.cato` implements the
loader (resolve, dedup by lexically-normalized path, canonical-per-file
rename); the seed implements the same semantics in `cato-core`. The
`_ref` drivers (`lib/stdlib_ref.cato`, `reader/reader_ref.cato`,
`hir/hir_ref.cato`, `emit/emit_ref.cato`) are test-only entries.

## The bootstrap test (`crates/cato-backend-interp/tests/kernel_bootstrap.rs`)

Two layers:

1. **CLI parity through the interp** — for every non-error fixture,
   `cato run selfhost/cato/cli.cato build <fixture> <out> runtime/runtime.c`
   exits 0; the produced binary runs and byte-matches `cato run` of the
   same fixture (`host` exempt on stdout for argv[0]; dump-mode
   fixtures without a `main` get a `clang -c` validity check instead of
   link/run, as in S7).
2. **The cato1 → cato2 chain** (`#[ignore]`) — `cato build
   selfhost/cato/cli.cato -o target/cato1` succeeds today (the seed's
   emission clangs); the ignored test runs the whole chain and is
   flagged back on once the blocker below is fixed.

## Bootstrap prerequisite: seed bugs surfaced by the kernel

Making the seed compile the kernel's own source (S7's recorded
prerequisite) turned into the phase's main event. Fixed in the seed:

- **Count of a cloned str** — `emit_int`'s `Count` arm assumed
  `(count (clone x))` wraps a vector; a str clone now takes the strlen
  path (the kernel clones defensively everywhere).
- **`parse-i64`/`parse-f64` in value position** — the emitter handled
  `ParseNum` only as a let-binding; a `(match (parse-f64 ...) ...)`
  scrutinee panicked `emit_adt`. It emits the runtime's Option box,
  with `adt_of` reading the node's `res` type.
- **`If` in `adt_of`** — an `if` whose branches return an ADT (the
  kernel's result-wrapper pattern) had no arm.
- **Block-label collision** — the emitter names blocks `b<n>`; the
  kernel's `em-ins2` has parameters literally named `b1`/`b2`, and a
  branch to `%b1` hit the parameter, not the block. `mangle` now
  escapes user locals matching the emitter's synthesized shapes
  (`t<n>`, `b<n>`, `entry`, plus prior `_u` escapes) — injective, so
  `b1` → `b1_u` can never collide with another user name.
- **Binder leaks past scopes** — the emitter's flat binding maps never
  restored shadowed names: sibling match arms saw each other's binder
  registers (the dominance errors recorded in S6), and a shadowing
  `let` silently changed which register a later read saw — interp
  printed `6 5`, native printed `6 6`. `let` bodies, match arms and
  loop variables now save/restore the binding maps around their extent
  (`BindScope` on `FnCtx`).
- **Duplicate defns** — the interp resolved duplicate top-level names
  last-wins while the emitter would define the symbol twice; the seed
  now rejects duplicates at build time.

## The alias discipline, now checked

The binder-leak fix exposed the deeper defect: binders of a
boxed-scrutinee `match` alias the scrutinee's single heap box (its
fields ride inside it), and nothing stopped a binder from escaping the
arm in tail position or being moved into an owning position — after
which the scrutinee's scope-exit free (or an owned callee's param free)
released the box under the escapee. The seed's own examples clone at
every boundary (`(insert (clone l) x)`) and were never affected; the
kernel's source did it ~90 times, invisibly (the interp has no drop
glue; the native build never linked until the fixes above).

The ownership checker now enforces the discipline: for a boxed
scrutinee, a binder whose field aliases (`str`, vec/map/set, boxed ADT
children — scalars and inline-ADT children copy by value) may not be
tail-returned or moved; the error names the binder and the fix (clone
it). Boxed-scrutinee binders carry their real field types so moves
register; the ADT table threads into `plan_drops`, and boxedness is
computed over the full template+instance table (match scrutinees carry
combined-table indices). The kernel source was swept mechanically
against the checker's spans — every flagged use wrapped with `(clone
...)` across `forms`, `lower`, `moveck`, `hexpr`, `emit` and the
`_ref` drivers — and the sweep is guarded by the S5/S6/S7
differentials staying byte-identical (232 tests).

## Remaining blocker: one UAF on the full kernel

*(Updated 2026-10-06 after the first root-cause pass.)*

### Root cause found: the checker never saw moves through temp-scrutinee binders

The ownership checker refined match binders to their real field types only
when the scrutinee was a named `Var` (`ownership.rs`, the Match arm). A
fresh-temp scrutinee — `(match (build-program forms tab) (POk [tab2 adts
defs lams sigs]) ...)` — left every binder at the `Ty::I64` copy stand-in,
so passing a binder to an owned parameter never registered as a move and a
second use of the same binder went unchecked. `compile-build` (cli.cato)
did exactly that: `adts` flows uncloned into `adts-templates` (whose owned
`ts` param frees the whole list at its scope exit) and is then passed again
to `emit-program`. `emit_ref.cato` and `hir_ref.cato` carry the identical
pattern.

The failure signature everyone was chasing — `clone__HExpr` /
`clone__AdtTab` / `clone__Sids` dereferencing NULL or `0xbe…` from inside
`lok` / `emit-program` — is downstream of this class: freed chain nodes get
reused or read as pointers. The interp cannot see any of it (no memory
model), and freed-but-unreused memory still reads correctly, which is why
the pipeline mostly worked natively before crashing late.

### Fixed

- **Checker** (`cato-core/src/hir/ownership.rs`): a new `scrutinee_adt`
  helper resolves the scrutinee's ADT for any shape (var → binding map;
  call → return types, threaded through `plan_drops` as a `rets` map from
  all six call sites; ctor/match/if/clone/Option-`res` per the backend's
  `adt_of`). Binders of boxed temp scrutinees now carry real field types;
  moves register; second uses error.
- **Kernel source sweep** (checker-driven, ~30 sites): lower-call,
  moveck's b-let-binds / b-loop-binds / b-arms / bcheck (HConj state
  reads, HIf branch states, HMatch, HLet b-frees, HCtor/HCall `c1`
  moves), emit's scalars/heap/adt/emit modules (the `em-cur`-then-
  `em-line` idiom, `labels` triple-use in the two match emitters,
  `emit-count`'s cond tests, `emit-lambda-pair` restructured so the ctor
  arg order stops using a moved binder), and the three drivers (cli,
  emit_ref, hir_ref — `(clone adts)` / `(clone tab2)` at the flagged
  uses).
- The S5/S6/S7 differentials stay byte-identical (full suite green with
  fresh debug binaries).

Note: `kernel_emit`/bootstrap tests using `target/debug/cato` ran against a
stale binary earlier; `cargo test` (debug) refreshes it — release-mode
`cargo test --release` does not, and its green was partly an artifact.

### Next probe (the remaining native-only crash)

cato1 now completes read → expand → lower → moveck and produces
byte-identical IR to the interp for `lets`/`scalars`/`strings`/`macros`
fixtures, and gets through clang for `atoms`. Fixtures with
boxed-ADT/closure state (`adts`, `closures`, `control`, `kitchen`, `mono`,
`threads`, `vectors`) still crash in the emit stage. Minimal repro
(`cato1 build` on):

```
(deftype Shape (Circle [r :- i64]) (Rect [w :- i64 h :- i64]))
(defn area [s :- Shape] :- i64
  (match s (Circle [r] (* r 3)) (Rect [w h] (* w h))))
(defn main [] :- i64 (let [c (Circle 7)] (area (clone c))))
```

ASAN on the minimal repro: `clone__Mdl` → `cato_str_vec_copy` reads an
Emit's symbol-table buffer that a *prior* `drop__Emit` freed — two Emit
states sharing one `mdl` (or a stale state register). Every state-thread
site in the emit module deep-clones on inspection; the source-level model
proves sound, which points at the seed emitter's handling of the emit
module's pervasive shadowed binder names (`c`/`mdl`/`sigs` at every
 nesting level) — the known "shadowed names share one env slot"
limitation in `fnctx.rs` is the prime suspect. The `kernel_emit` test
cannot catch this class: it drives the kernel's emitter through `cato run`
(interp), so the emitted natively-executing kernel code is only ever
exercised by cato1 itself. A native-driver differential (run the seed-
compiled S7 kernel on the fixture corpus and compare against the interp)
would guard this layer.

## Non-goals

- Byte-matching the seed's `--emit-ir` on the kernel (S7's stance).
- Kernel-CLI conveniences beyond the skeleton: diagnostics with spans,
  `--cc`/`-O` flags, incremental compilation, `run` subcommand (the
  parity harness runs the produced binaries directly).
- The middle end for the kernel (S9+).
