# S7 — Kernel LLVM emitter (`selfhost/cato/emit/`)

Date: 2026-09-20
Goal: the kernel turns its S6 typed IR into textual LLVM IR — a Cato port
of the seed emitter's *shape* for the subset contract, not its full feature
set. Every construct the S6 lowering produces must emit; everything outside
the contract never reaches the emitter (the lowering rejects it first).
Acceptance: emit → `clang` → run over a growing program suite, byte-comparing
stdout with the seed interpreter over the same program; where the subset
overlaps the seed, `--emit-ir` output is an informational signal (see
Non-goals).

## Inputs and stage

The emitter consumes exactly what `check-program` returns: the post-moveck
program — `adts` (combined template+instance table), `defs` (plain in
source order, then externs, then mono instances — the seed's out order),
`lams` (creation order), `sigs` (closure signature table), plus the intern
table for name rendering. Frees lists, conj `in_place` bits and closure
capture own-bits are already computed in the IR; the emitter is a dumb
consumer of them, exactly like the seed consumes `plan_drops` output.

Representation facts (`boxed`/`owns`) are recomputed by the emitter over
the **full combined ADT table** — instances included. This differs from
S6's move-check facts (templates only, mirroring the seed's build-stage
plan_drops on the pre-instance table); the seed's own emitter recomputes
`compute_adt_facts` over the full table in `Mdl::new`, and the kernel
mirrors that. The kernel's `facts-compute` (Warshall + owns fixpoint) is
already the general computation; only its input changes.

## Output shape (the seed's, for the subset)

- module header comment; `%VecI64 = type { ptr, i64, i64 }`;
  `%Closure = type { ptr, ptr, ptr }`; one `%<name> = type { i64, [N x i64] }`
  per ADT in combined-table order (`N` = payload slots);
- declares: `malloc`/`free`/`strdup`/`cato_str_vec_copy`/`cato_str_vec_free`/
  `llvm.memcpy` up front, plus the print trio; runtime helpers discovered
  during emission are deduplicated into the same up-front block (a chain
  scan replaces the seed's `Vec::contains`);
- deep-clone glue `@clone__<adt>` for boxed ADTs, drop glue `@drop__<adt>`
  for owning ADTs (boxed = owns in the revised model; the two facts
  coincide);
- one `define` per plain defn (main keeps the i32 ABI with
  `@cato_args_init` plumbing and `ret i32 0`), one `declare` per extern,
  one `@lambda_N` + `@drop__lambda_N` pair per closure;
- vector/string literals as private constants `@g_vec_<k>`/`@g_str_<k>`,
  minted in expression order, collected at module top.

## Emission state (the S4/S5/S6 law: thread state, return it in results)

- `Mdl` — global counter (literal global numbering), globals buffer,
  declared-lines chain, the ADT table, and the full-table facts. Threads
  through every emitter call; `ensure-decl` returns the updated `Mdl`.
- `FnCtx` — temp counter, block counter, current block, the body `StrBuf`,
  the hoisted-entry prelude `StrBuf` (spliced after `entry:` at assembly —
  the seed's `with_prelude`), and the binding environments: `scalar`
  (sid → register + `Ty`: ints/floats/bool/str/ADT/closure slots),
  `vecs` (sid → read-only `(ptr, len, elem)`), `grow` (sid → growable
  `%VecI64` header slot + elem), and the innermost loop frame
  (slots + head label; saved/restored around nested loops, exactly the
  seed's `loop_frame`).
- Environments are `(AList V)` keyed by name sid with prepend-on-bind —
  first-match-wins reads reproduce the seed's map-overwrite semantics for
  shadowed binders (each arm/let body is emitted entirely after its bind,
  so the most recent put is the visible one).
- Per-node results are result ADTs: `SRes` (a value register), `VRes`
  (a `(ptr, len, elem)` pair), `HRes` (a `(ptr, len, cap)` header for
  field stores), `URes` (statements). Clones at the call boundaries are
  the documented S5 stance.

## Contexts (the seed's dispatch, mirrored)

`emit-int` / `emit-float` / `emit-bool` / `emit-str-val` / `emit-adt` /
`emit-vec-pair` dispatch on the node kind per context, matching the seed
arm for arm — including:

- bool binding in i64 position widens with `zext`; `ConstI64` in f64
  context `sitofp`s; match arm values pin through `fadd`/`xor`/`add 0`
  before the phi; `even?` is the `and 1` + `icmp eq 0` pair; string
  equality/order ride `strcmp`; `print-i64`/`print-f64` widen narrow
  args (`static-int-width`, ported) exactly like the seed;
- casts: same-bits int reinterpretation, trunc/sext/zext, sitofp/uitofp/
  fptosi/fptoui, fptrunc/fpext — the seed's `emit_cast` table;
- `get` is the branchless bounds-checked load (clamp with selects,
  `icmp ult` against the length); the two-argument forms materialize
  `(Option T)` inline slots via `select` on the in-bounds flag;
  `parse-i64`/`parse-f64` return the runtime's 16-byte box, which the
  match treats as a pointer to the inline `{tag, payload}` layout;
- `let` binds per storage class: heap constructors (`conj`) as growable
  slots, `split`/vector-returning calls as read-only vec pairs, `args`
  through `@cato_args` with the out-param alloca, Option lookups and
  parse boxes as scalar ADTs, closures as `%Closure` allocas, scalars
  and strings as registers — the seed's `emit_let` decision table;
  frees run at scope exit (skipped on the all-recur paths of a loop
  tail, like the seed);
- `if`/`match` phis per type (ADT/str as `ptr`); `match` switches on the
  tag with the wildcard arm as default, `unreachable` for the
  exhaustiveness hole, field loads binding arm binders per their types
  (inline ADT children alias the parent's embedded region — values are
  immutable); `:when` guards arrive pre-desugared as nested matches;
- `loop`/`recur`: entry-hoisted per-variable allocas, fresh loads at the
  head, `recur` stores + branches back, the loop-tail walk distinguishing
  completing from recursing paths, `unreachable` when every path recurs;
- ADT constructors: boxed ADTs `malloc` the struct (the value is the
  owning pointer), inline ADTs alloca an entry slot; fields store at
  their slot offsets — strings strdup, vectors spread `(ptr,len,cap)`
  across three slots with an owned deep copy, boxed children
  `ptrtoint`, inline children copy whole; boxed/owning facts decide;
- closures: env block = ownership bitmap word + captures in slot order
  (1 slot scalars/str/boxed ADT, 3 slots collections and captured
  closures); creation, indirect call (env-first), the `@lambda_N`
  emission (captures loaded back out of the env; bools ride i8 with a
  trunc) and the bitmap-guarded `@drop__lambda_N` — the seed's
  phase-14 convention verbatim so emitted symbols and layouts stay
  interchangeable;
- host builtins: `cato_print_i64`/`cato_print_f64`/`cato_print_string`/
  `cato_print_str_raw`/`cato_slurp`/`cato_spit`/`cato_args`/`cato_exit`/
  `cato_num_to_str`/`cato_u64_to_str`/`cato_f64_to_str`/
  `cato_parse_i64`/`cato_parse_f64`/`cato_str_split`/`cato_str_join` —
  the same runtime.c surface the seed links.

## f64 literals (`llvm-f64`)

The seed formats Rust `{:e}` and fixes up the decimal point. The kernel
has no `{:e}`; `(str f)` is the interp's 6-significant-digit `%g`, which
does not round-trip. The kernel instead computes a **shortest round-trip
decimal**: for increasing scale `p` (0..=17 digits after the point),
render `floor(v·10^p)` and `ceil(v·10^p)` as fixed-point decimals,
`parse-f64` each candidate back, and keep the first that equals `v`
(sign handled once, zero short-circuits to `0.0`). A `.0` is appended
when the chosen text lacks a fractional part — LLVM requires the decimal
point (the same rule the seed's comment records). Exactness matters:
the emitter's constants feed S9's fixpoint comparison, and lossy 6-digit
encodings would change program behavior.

## Driver (`emit/emit_ref.cato`) and test

The driver concatenates after the S4/S5/S6 files and defines the only
`main`: `<file> emit` reads, expands, builds, move-checks, emits, and
prints the module text (build errors print `err ...` as in S6). It shares
`magic-table`/`expand-all`/`build-program`/`check-program` with the S6
driver but is a separate concat + harness so S6 stays untouched.

The test (`crates/cato-backend-interp/tests/kernel_emit.rs`) follows the
S6 harness pattern: canonical concat → `target/`, driven by `cato run`
(the kernel driver still runs through the seed interpreter — the seed's
own emitter miscompiles the full kernel concat with a dominance error,
recorded in S6; fixing it is S8's bootstrap prerequisite, not S7's).
Per fixture: capture the emitted IR, `clang` it with `runtime/runtime.c`,
run the binary, and byte-compare stdout with `cato run` of the same
fixture through the seed interpreter. `host.cato` is compiled, run, and
required to succeed, but its stdout is exempt from the comparison: the
interp's `(args)` excludes argv[0] by S3 design while the native binary
sees the full argv, so the two sides legitimately disagree on
argv-derived output (kernel-CLI argv parity is S8's driver work).
Error fixtures (`err_*`) are excluded: out-of-contract input never
reaches emission.

## Non-goals

- Byte-matching the seed's `--emit-ir`: the seed's output covers the
  post-optimization program (fold/DCE/devirt/inline rewrite the IR the
  emitter sees), so byte equality would drag in the whole middle end.
  The comparison stays a manual/development-time signal; the enforced
  contract is clang-validity plus interp↔native stdout parity.
- The middle end, fused `reduce` loops, parallel runners, maps/sets,
  wasm target, `--fast` metadata.
- Fixing the seed emitter's dominance bug on the kernel concat (S8).
- Diagnostics with spans in emitted-output errors (the emitter panics
  via the interp's failure path on impossible states, same as the seed's
  `panic!("codegen: ...")`).
