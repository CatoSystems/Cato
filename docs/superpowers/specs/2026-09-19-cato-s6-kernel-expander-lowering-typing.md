# S6 — Kernel expander, lowering, typing (`selfhost/cato/hir/`)

Date: 2026-09-19
Goal: the kernel turns the S5 AST into the seed's typed HIR — for the subset
contract. The fixed macro set expands exactly as `crates/cato-core/src/
expand.rs`; `defn`/`extern`/`deftype` parse exactly as `hir/parse.rs`; the
expression grammar lowers to a typed IR matching `hir/lower.rs` +
`hir/forms.rs` + `hir/ctl.rs` + `hir/pipeline.rs` node for node; move
checking + drop planning mirror `hir/ownership.rs`; generic `defn`s and
ADTs monomorphize with the seed's mangled-name scheme (`hir/mono.rs`,
`hir/mod.rs`). Acceptance: a differential test runs the kernel build
(expand → parse → lower → move check) over a fixture corpus and the kernel's
typed-IR serialization is byte-identical to a Rust oracle serializing the
seed's `build_program_located` output (the pre-optimization `--dump-hir`
stage) with the same grammar.

## Stage alignment

The seed's `--dump-hir` prints the Rust `{:#?}` of the post-`compile`
program. Byte-matching Rust's Debug from Cato is meaningless; per the repo
method (S5) the differential oracle is a serializer with the same wire
grammar over the same pipeline stage:

- oracle: `read_all_spanned(PRELUDE + src)` → `expand_all_located` →
  `build_program_located` — the pre-optimization HIR, before `fuse::check`,
  before `opt`, before the second `plan_drops(true)` pass;
  `build_program_located` itself runs `plan_drops(false)`, so `frees`
  lists, `in_place` conj bits and `capture_owns` are already computed —
  the kernel move check must therefore produce them identically.
- kernel: read (S5) → expand → build (forms → lower → move check) → dump.

`PRELUDE` contributes the `(Option a)` deftype (adt 0) and the `print`
multimethod — whose forms `collect_generics` strips (no `print` call sites
in the corpus). The kernel pre-seeds exactly the `Option` template as adt 0
and rejects `defgeneric`/`defmethod` (`cato1: unsupported form defgeneric`).

## The subset (what lowers; everything else fails `cato1: unsupported ...`)

- Top level: `defn`, `extern`, `deftype`. Not `defgeneric`, `defmethod`,
  `ns`, `require`, `reduce`/`for`/`doseq`/`parallel`, map/set literals or
  ops (`{}`/`#{}`/`assoc`/`disj`/`contains?`), `subvec`, `zip`.
- Macros (expander): `->` `->>` `when` `when-not` `cond` `if-let`
  `when-let` `if-not` `and` `or` — byte-matching the seed's desugaring,
  including `(and a b c)` → `(and a (and b c))` and `cond`'s odd-tail
  default. `doseq` is outside the contract (seed-only macro).
- Expressions: literals (i64/f64/bool/str; `[1 2]`-style numeric vector
  literals; `vec-i64`/`vec-f64`), `do`, `let` (ascription + vector
  destructuring), `if`, `and`/`or`/`not`, `abs`/`min`/`max`,
  arithmetic/comparisons (incl. string `=`/`<`/... via runtime),
  width casts (`i8`..`usize`, `f32`, `f64`), string ops (`str`, `subs`,
  `split`, `join`, `nth`, `char->str`, `count`), vector ops (`get`,
  `conj` with the seed's literal folds), `match` (+ `:when` guards),
  `loop`/`recur`, closures (`fn`, closure calls), ADT constructors,
  `clone`/`drop`, calls, `slurp`/`spit`/`args`/`exit`,
  `parse-i64`/`parse-f64`, `print-i64`/`print-f64`/`print-str`/
  `print-string`, `even?`.

## Kernel-side representation

### Types are flat words

`Ty` is `(Ty [tag :- i64 a :- i64])` — unboxed, so type values thread by
copy and never clone: tag 0..14 are the scalars/`str`/`map`/`set`/`none`
(absent-`Option` sentinel), 15 `TVec` (`a` encodes the element: 0 i64,
1 f64, 2 str, 100+k param k), 16 `TAdt(i)`, 17 `TFn(i)`, 18 `TParam(i)`.
Type *equality* is tag+a equality; the seed's `Ty::Vec(Elem)` distinction
(Elem::Param vs Ty::Param) is kept in the `a` encoding. All lists of types
are chains (`TyList`), not vectors — the seed's deftype field grammar
allows vector elements over scalars only, so no table in the kernel can be
a `(Vector Adt)`.

### Names are sids; the symtab is the reader's

The driver pre-interns every special name (the macro heads, special-form
heads, builtin heads, `$b`) before reading the file — `scan` is seeded
with that table via a `read-all-with` wrapper, so the reader's interning
continues it. Synthesized names (`$d0`, `_wild0`, `lambda_0`) mint through
`intern2` into the same table. The dump renders names through the final
table; sids never cross the wire (S5's rule).

### The typed IR (`hexpr.cato`)

One recursive `HExpr` ADT with child chains:

- `HExprs` — ordered children (call args, `str` args, `Seq`, ctors);
  `HENil` doubles as the absent-`Option` child (e.g. `Get`'s default).
- `Binds` — let/loop bindings `(name sid, value HExpr, ty)`; match arms
  are `HArms` of `(variant, binders Sids, body)` with variant −1 for `_`.
- `Caps` — closure captures `(name, ty, own)` (the seed splits `captures`
  and `capture_owns`; one cell is the same information).
- Optional `Ty`s (the `res` of two-argument `get`/`nth`/`parse-*`) ride as
  `TNone`.

Every seed `HExpr` arm in the subset has a mirror variant with the same
fields; `Frees` are `Sids` on `HLet`. Synthesized nodes carry the span of
the enclosing source form (the seed's IR has no spans; they never cross
the wire — they exist for S7 diagnostics).

### Program tables (`forms.cato`, threading state)

- `Raws` — `(AList RawDefn)` keyed by name sid; `RawDefn` holds the parsed
  header (params, ret, typarams, extern flag) and the raw body `Ast`.
  Duplicate names: last put wins (`al-get` first-match on a prepending
  list = the seed's HashMap last-insert-wins).
- `Adts` — declared templates then instances, index = position;
  `AdtInfo` holds name sid (instances: the mangled `Name__arg` string
  interned), typarams, variants, and for instances the template index.
  Instances are keyed by (template, args) in an `AList` for dedup.
- `Insts` — the monomorphization cache: mangled-name sid → ret, plus the
  instance defns (lowered bodies) in creation order, plus the `ncached`
  counter (it feeds the `$d`/`_wild` gensym numbering — see below).
- `Sigs` — the closure-signature table `(params TyList, ret)` in creation
  order (annotation-time registrations during form parsing first, exactly
  as the seed's pass-1 `parse_ty` does, then closure-time).
- `Rets` — name sid → ret for `ty_of` (static defns at parse time, mono
  rets appended at instantiation).

State threads as parameters and returns inside results — the S4/S5 law:
a heap parameter is consumed by the callee, so a caller that retains it
passes `(clone T)`; functions that grow tables return them in a result
ADT (`LRes`). The known O(nodes × state) clone corners are the documented
S5 stance: fine at kernel-source scale, revisited if S8 profiles complain.

## Lowering — the seed's decisions, mirrored exactly

These are the places where the seed's *order* is observable in the output;
the kernel copies them mechanically:

- gensym numbering: destructuring temporaries are `$d{n}` with
  `n = ncached + resolved-so-far`; wildcard match binders are
  `_wild{n}` with `n = binders-so-far + ncached`; lambdas are
  `lambda_{n}` by creation order (creation = after the body lowers, so
  inner closures precede outer). The `ncached` counter advances exactly
  where the seed inserts into `InstState::cache` — at the call site,
  *before* the instance body lowers — so numbers interleave identically.
- the three-argument `nth` desugars to a `match` over the two-argument
  form with binder `$b`.
- guard arms re-enter lowering with the rewritten arm list plus a routing
  `_` arm, after re-lowering the scrutinee as a probe (the seed lowers it
  twice on the guard path).
- constructor inference: field-by-field unification against the template
  (`expected` from the ascription pins nullary ctors through the instance
  reverse table); instances are minted at the ctor, after the last field.
- `Call` ret typing: raw non-generic ret, else mono cache, else the i64
  fallback (`print-i64` et al. are not in any table — the fallback *is*
  the seed's answer).
- closures: `fn` params are `parse_ty_sym`-only (no ADT/vector/`Fn`
  annotations — the seed passes an empty ADT table there); captures are
  the lowered body's free vars in first-reference order (the seed's
  `collect_captures` walk, arm order copied); sigs dedup structurally.

## Move check + drop planning (`moveck.cato`)

A port of `BCheck` over the subset: per-name ownership states, `remaining`
occurrence counts, tail-escape set, shadowed-bindings pre-pass, consumed
set; the same merge rules for `if` branches and `match` arms (union of
moved/poisoned states); conj's last-use `in_place` bit; closure captures
consuming non-Copy vars with per-capture `own` bits; `let`'s
transfer-on-Var-init ownership rule; scope-exit `frees` = heap-owning ∧
still-owned ∧ not escaped ∧ not consumed ∧ not shadowed. The kernel has
no borrows, so `no_move` is empty and every non-extern defn's params are
all-`Owned` (externs all-`Shared` — the seed's own convention). Errors
reuse the seed's wording (`use of moved value x`, `ownership check failed
in f: ...`).

`compute_adt_facts` (cycle boxing + owns fixpoint) ports as the facts
source (`facts.cato`), iterative DFS like the seed.

Monomorphization (`mono` inside `lower.cato`): unify arg types against
the raw params (`subs` as an `(AList Ty)`), substitute param/element/ADT
types, mangle `name__arg1_arg2` with the seed's `ty_suffix`/ADT-name
suffixes, register in the cache before lowering the body, append the
instance defn after. Recursive generic calls resolve to the registered
name because registration precedes body lowering — same as the seed.

## Dump grammar (`dump.cato` — the wire)

One line per record; bodies as s-expressions; names rendered through the
symtab; f64 through the S5 `%g` contract (`(str f)` / interp `fmt_f64`):

```
program := (adt ...) * (defn ...) * (lambda ...) * (fnsig ...) *
adt     := (adt i name params:[T ...] variants:[(v [T ...]) ...])
           ; i is the combined-table index; instances carry their
           ; mangled name and template index: (adtinst i tpl name ...)
defn    := (defn name [p:T ...] :- T ext:0|1 body)
lambda  := (lambda mangled [c:T:own ...] [p:T ...] :- T body)
fnsig   := (fnsig [T ...] T)
T       := i64 | i32 | i8 | i16 | u8 | u16 | u32 | u64 | usize | f64
         | f32 | bool | str | map | set | (Vector e) | (Adt i)
         | (Fn i) | (Param i)
body    := (const n) | (f64lit g) | (bool 0|1) | (str esc) | (var name)
         | (binw op a b T) | (binf op a b) | (cmp op a b T)
         | (icast e from to) | (veclit e [v ...]) | (strlit esc)
         | (strcat e ...) | (tostr e T) | (subs s a b) | (split s sep)
         | (join sep xs) | (nth s i Tres) | (charstr c) | (strcmp p a b)
         | (strord op a b) | (count e) | (get v i [d] T Tres)
         | (conj v x e in_place) | (cat e ...) | (if c t e T)
         | (and a b) | (or a b) | (not e) | (abs e T)
         | (min a b T) | (max a b T) | (seq e ...) | (let [n e T ...]
           frees:[n ...] body) | (loop [n T e ...] T body)
         | (recur e ...) | (ctor a v e ...) | (match scrut T arm ...)
         | (arm v [b ...] body) | (clone e) | (drop e)
         | (closure sig mangled [p:T ...] [c:T ...] body)
         | (closurecall f sig e ...) | (call name e ...)
         | (slurp p) | (spit p d) | (args) | (exit n)
         | (parse s int Tres)
```

Empty sections print nothing; `ext:1` defns print with body `(const 0)`
(the seed's placeholder). `Tres` absent renders `none`. The seed side of
the oracle renders `Program` fields in the same order the kernel builds
them: adts (declared then instances), defns (plain in source order, then
externs, then mono instances), lambdas (creation order), fn_sigs.

## Fixtures (`selfhost/cato/hir/fixtures/`)

One file per feature cluster, all inside the contract: `scalars`
(arithmetic, widths, casts, comparisons, bool ops), `lets` (ascription,
destructuring, shadowing, move/clone/drop), `strings` (str/subs/split/
join/nth/parse/char->str/count), `vectors` (literals, get 2/3-arg,
conj folds), `control` (if/when/cond/and/or expansion results, loop/
recur incl. non-tail error... no — errors are not in the byte contract;
loop/recur valid shapes), `adts` (deftype, ctors, match + guards,
Option/get), `closures` (captures incl. non-Copy, closure calls),
`mono` (generic defn over i64/f64/str/vec + generic ADT instantiation,
mangled names), `host` (slurp/spit/args/exit/print builtins), plus
combinations (`kitchen`). The driver errors on out-of-contract forms —
pinned by kernel-side expectation, not by the seed byte contract.

## Differential test (`crates/cato-backend-interp/tests/kernel_hir.rs`)

The S5 harness pattern: canonical concat (S4 lib, S5 reader, S6 hir
files, `hir_ref` driver) written to `target/`, built once by the seed
into a native binary, run per fixture; the oracle serializes
`build_program_located` over `PRELUDE + fixture source` with the same
grammar (f64 through `cato_interp::fmt_f64`). Byte-identical per file,
or the test names the fixture. Backend parity: the same concat under
`cato run` (interp) must agree with the native binary over the whole
corpus — the expander/lower/moveck are exercised on both backends from
day one. The expander additionally gets its own differential (mode
`ast`: dump the expanded forms with the S5 node grammar against a
seed `expand_all_located` oracle) before the IR pipeline lands.

## Non-goals

- The middle end (fold/DCE/devirt/inline/fusion check) — S7+ concern;
  the oracle deliberately compares the pre-optimization stage.
- Borrow checking (`&` fails as unsupported), multimethods, modules,
  map/set representations.
- Byte-matching the seed's *error* strings on out-of-contract input —
  the contract there is `cato1: unsupported ...`, never a miscompile.
  In-contract error wording (move errors) mirrors the seed.
- Performance: the threaded-state clone corners are documented, not
  optimized.
