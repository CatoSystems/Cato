# S8 — Stage 1: the kernel builds itself (`selfhost/cato.cato`)

Date: 2026-10-02
Goal: the kernel gets a CLI and the bootstrap chain stands up — seed
builds cato1, cato1 builds cato2, cato2-built binaries run the reference
programs in parity with `cato run`. Acceptance per the ROADMAP: a kernel
CLI skeleton (read source → compile → write `.ll` → spawn `cc` → native
binary, with real exit codes), and the cato1 → cato2 link closing.

Status at the end of this phase: **the CLI skeleton and the bootstrap
harness are complete and green through the seed interpreter; the
cato1 → cato2 leg is built and blocked by one remaining seed-emitter
use-after-free on the full kernel concat** (details below). Everything
else the phase needed — including six seed compiler bugs that stood
between the seed's LLVM backend and the kernel's own source — is fixed
and guarded.

## The kernel CLI (`selfhost/cato/cli.cato`)

Concatenated after the lib, reader, hir and emit files; defines the
only `main` of the cato1 concat. The skeleton:

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

## The canonical concat (`selfhost/cato.cato`)

The kernel subset has no `require`, so the compiler's own source is one
namespace. `tools/concat_kernel.py` writes `selfhost/cato.cato`: the
lib, reader, hir and emit files in the S7 harness's canonical order,
closed by `cli.cato` (the `_ref` drivers stay test-only). The committed
file is the distribution form; `kernel_concat_is_fresh` regenerates and
byte-compares on every test run so it cannot rot.

## The bootstrap test (`crates/cato-backend-interp/tests/kernel_bootstrap.rs`)

Three layers:

1. **Freshness** — the committed concat matches the generator.
2. **CLI parity through the interp** — for every non-error fixture,
   `cato run selfhost/cato.cato build <fixture> <out> runtime/runtime.c`
   exits 0; the produced binary runs and byte-matches `cato run` of the
   same fixture (`host` exempt on stdout for argv[0]; dump-mode
   fixtures without a `main` get a `clang -c` validity check instead of
   link/run, as in S7).
3. **The cato1 → cato2 chain** (`#[ignore]`) — `cato build
   selfhost/cato.cato -o target/cato1` succeeds today (the seed's
   emission clangs); the ignored test runs the whole chain and is
   flagged back on once the blocker below is fixed.

## Bootstrap prerequisite: seed bugs surfaced by the kernel concat

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

## Remaining blocker: one UAF on the full concat

With all of the above, the seed compiles the concat and the produced
cato1 binary clangs and runs — but crashes while compiling its own
source, in the build phase's ADT-table threading. Evidence gathered:

- `clone__Sids` dereferences a NULL/`0xbe…` pointer; under ASAN the
  poison pattern shows the `Sids` box was freed before the read.
- The chain is `emit_program` → `(facts-owns-of (clone adts))` →
  `clone__AdtTab` → `clone__AdtInfo` → `clone__Sids(NULL)`: an
  `AdtInfo` with a NULL/`0xbe` `tps` field inside a live `AdtTab`.
- `build_lower_2` receives a valid `adts` at every entry (guarded), so
  the free happens somewhere between the table's construction and
  emit-program's clone.
- Every static candidate checked sound: `adts-append-info`/`adts-set`
  clone their inputs before dropping the originals; the match
  scrutinee temps of `build-program`'s pipeline leak (never dropped),
  so their field aliases are valid; `ps-adts`-style accessors return
  clones; param frees suppress on moved/returned params.

The next probe is a hardware watchpoint on a live `AdtInfo`'s payload
or an ASAN build of the *seed* itself (the free likely happens inside a
seed-emitted drop-glue sequence the source-level analysis mis-models).
This is the only gap between here and `cato1 build selfhost/cato.cato`
producing a working cato2.

## Non-goals

- Byte-matching the seed's `--emit-ir` on the concat (S7's stance).
- Kernel-CLI conveniences beyond the skeleton: diagnostics with spans,
  `--cc`/`-O` flags, incremental compilation, `run` subcommand (the
  parity harness runs the produced binaries directly).
- The middle end for the kernel (S9+); multi-file `require` (post-
  fixpoint).
