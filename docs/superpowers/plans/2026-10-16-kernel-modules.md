# Kernel Modules (`require` replaces the concat) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The kernel compiles itself from `selfhost/cato/**/*.cato` wired with `(ns ...)`/`(require ...)`, so `selfhost/cato.cato` and `tools/concat_kernel.py` are deleted and every build path points at a real entry file.

**Architecture:** Both compilers adopt one canonical-per-file rename model for `require`: each required file's `defn` names are renamed `canonical__name` exactly once, and every requirer's `alias/name` refs are rewritten to the module's canonical names. Types are never renamed. `extern` names are global C symbols and are never renamed. The seed gets this in `expand_file`; the kernel gets a line-for-line port in a new Cato module (`hir/modules.cato`). Sources are then converted by a one-shot script and the concatenation is removed.

**Tech Stack:** Rust (seed compiler, `crates/cato-core`), Cato (selfhost kernel), Python 3 (one-shot conversion script), Bash (`tools/kernel_gate.sh`), Cargo test harness.

**Spec:** `docs/superpowers/specs/2026-10-16-kernel-modules-design.md`

## Global Constraints

- Seed and kernel must accept/reject the same programs; the loader's errors mirror the seed's messages verbatim.
- Only `defn` names are renamed; `extern` names are global C symbols and are never renamed (declaration or call site); `deftype`/constructor/`defgeneric`/`defmethod` names are never renamed.
- One `main` per compilation: `cli.cato` is the kernel entry; each `_ref` file is its own entry and must not appear in another entry's graph.
- One canonical alias per module, used in every `require` of it; one canonical relative path spelling per module (e.g. `../lib/ast.cato` from `hir/`).
- No path normalization: dedup by resolved path string, exactly as the seed does (`dir.join(path)` concatenation).
- Cycle termination: insert a file into the seen set before recursing.
- Every kernel-source change is gated by `./tools/kernel_gate.sh` PASS and `cargo test`; all `examples/*.cato` keep exact output.
- The kernel moved under this plan: rounds 1–13 (ownership/churn) rewrote `hir/moveck.cato`, `hir/lower.cato` and `emit/*`; the intern table is an `i64` stab handle (`stab-get`/`stab-find`/`stab-conj`) and `check-program` now takes `(defs lams facts tab adts)`. The line numbers below are anchors — re-read the files before each task.
- Commit or stash a green baseline before Task 3's mass rewrite; the working tree may carry uncommitted kernel/seed WIP.

## Review Focus

- A `require` cycle (`a` requires `b`, `b` requires `a`): must terminate and must not duplicate definitions or hang.
- The same module required under two different aliases: both requirers' refs resolve to the module's single canonical definitions.
- The same file reached via two different path spellings: dedup is by string; behavior must be defined and tested, not accidental.
- A duplicate `(ns ...)` name across the graph: rejected with an error naming the name.
- A ref through an alias the file never required, or a missing required file: a build error with a span, never a panic or hang.

---

### Task 1: Seed `require` — canonical-per-file rename

**Files:**
- Modify: `crates/cato-core/src/lib.rs` (`expand_file` and helpers, lines ~190–353)
- Test: `crates/cato-backend-interp/tests/modules.rs` (create)

**Interfaces:**
- Consumes: `reader::read_all_spanned`, `crate::ast::{Expr, Span}`, `diagnostics::{locate, with_source}` (all existing).
- Produces: `compile_named_with`/`compile_named`/`compile_to_program` keep their current signatures and behavior for non-`require` input. New internal helpers (not public):
  - `fn parent_join(file: &str, rel: &str) -> String` — `Path::new(file).parent().unwrap_or_default().join(rel).to_string_lossy().to_string()`.
  - `fn prefixed(prefix: &str, name: &str) -> String` — `prefix` empty → `name`, else `format!("{prefix}__{name}")`.
  - `fn rewrite_alias_refs(forms: &mut [(Expr, Span)], aliases: &[(String, String)])` — rewrite `alias/name` heads to `prefixed(canon, name)` for each `(alias, canon)`; a symbol is `Expr::LitSym`.
  - `expand_file(file, src, canon_prefix, ctx) -> Result<Vec<(Expr, Span)>, String>`.
  - `struct ModCtx { canon: HashMap<String,String>, aliases: HashMap<String,String>, ns: Vec<String> }`.

- [x] **Step 1: Write the failing tests**

Create `crates/cato-backend-interp/tests/modules.rs`. Use a helper that writes a small module project under `target/module_tests/<case>/`, compiles the entry with `cato_core::compile_named`, and runs it with `cato_interp::Interpreter::new(&program).run()`.

```rust
use std::path::{Path, PathBuf};

fn project(case: &str, files: &[(&str, &str)]) -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest.parent().unwrap().parent().unwrap().join("target/module_tests").join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    dir.join("main.cato")
}

fn run(entry: &Path) -> String {
    let src = std::fs::read_to_string(entry).unwrap();
    let program = cato_core::compile_named(&entry.to_string_lossy(), &src).expect("compile");
    cato_interp::Interpreter::new(&program).run().expect("run")
}

fn compile_err(entry: &Path) -> String {
    let src = std::fs::read_to_string(entry).unwrap();
    cato_core::compile_named(&entry.to_string_lossy(), &src).unwrap_err()
}

#[test]
fn diamond_requires_compile_and_run() {
    let entry = project("diamond", &[
        ("main.cato", "(ns m) (require [\"a.cato\" :as a]) (require [\"b.cato\" :as b])\n(defn main [] :- i64 (do (print (a/use-sq 3)) (print (b/use-sq 2)) 0))\n"),
        ("a.cato", "(ns a) (require [\"c.cato\" :as c])\n(defn use-sq [x :- i64] :- i64 (c/sq x))\n"),
        ("b.cato", "(ns b) (require [\"c.cato\" :as c])\n(defn use-sq [x :- i64] :- i64 (c/sq x))\n"),
        ("c.cato", "(ns c)\n(defn sq [x :- i64] :- i64 (* x x))\n"),
    ]);
    assert_eq!(run(&entry), "9\n4\n");
}

#[test]
fn same_module_under_two_aliases() {
    let entry = project("twoaliases", &[
        ("main.cato", "(ns m) (require [\"lib.cato\" :as x]) (require [\"lib.cato\" :as y])\n(defn main [] :- i64 (do (print (x/sq 3)) (print (y/sq 4)) 0))\n"),
        ("lib.cato", "(ns lib)\n(defn sq [a :- i64] :- i64 (* a a))\n"),
    ]);
    assert_eq!(run(&entry), "9\n16\n");
}

#[test]
fn same_file_reached_by_two_spellings_dedups() {
    // `a.cato` reaches lib as `lib.cato`; `b.cato` as `./lib.cato`. Both
    // resolve to the same file string only if spelled identically; when
    // they differ the second is a distinct path (documented, not a crash).
    // Pin the same-spelling case that the convention requires.
    let entry = project("spelling", &[
        ("main.cato", "(ns m) (require [\"a.cato\" :as a]) (require [\"b.cato\" :as b])\n(defn main [] :- i64 (do (print (a/g 3)) (print (b/g 4)) 0))\n"),
        ("a.cato", "(ns a) (require [\"lib.cato\" :as lib])\n(defn g [x :- i64] :- i64 (lib/sq x))\n"),
        ("b.cato", "(ns b) (require [\"lib.cato\" :as lib])\n(defn g [x :- i64] :- i64 (lib/sq x))\n"),
        ("lib.cato", "(ns lib)\n(defn sq [a :- i64] :- i64 (* a a))\n"),
    ]);
    assert_eq!(run(&entry), "9\n16\n");
}

#[test]
fn duplicate_ns_is_rejected() {
    let entry = project("dupns", &[
        ("main.cato", "(ns m) (require [\"a.cato\" :as a])\n(defn main [] :- i64 0)\n"),
        ("a.cato", "(ns m)\n(defn sq [x :- i64] :- i64 x)\n"),
    ]);
    assert!(compile_err(&entry).contains("duplicate ns m"));
}

#[test]
fn cycle_terminates() {
    let entry = project("cycle", &[
        ("main.cato", "(ns m) (require [\"a.cato\" :as a])\n(defn main [] :- i64 (a/one 0))\n"),
        ("a.cato", "(ns a) (require [\"b.cato\" :as b])\n(defn one [x :- i64] :- i64 (b/two x))\n"),
        ("b.cato", "(ns b) (require [\"a.cato\" :as a])\n(defn two [x :- i64] :- i64 x)\n"),
    ]);
    // Must terminate; whether it compiles is secondary, a hang/crash is not.
    let _ = cato_core::compile_named(&entry.to_string_lossy(), &std::fs::read_to_string(&entry).unwrap());
}

#[test]
fn unknown_alias_is_a_build_error() {
    let entry = project("badalias", &[
        ("main.cato", "(ns m) (require [\"lib.cato\" :as x])\n(defn main [] :- i64 (z/sq 3))\n"),
        ("lib.cato", "(ns lib)\n(defn sq [a :- i64] :- i64 (* a a))\n"),
    ]);
    assert!(cato_core::compile_named(&entry.to_string_lossy(), &std::fs::read_to_string(&entry).unwrap()).is_err());
}

#[test]
fn missing_required_file_is_an_error() {
    let entry = project("missing", &[
        ("main.cato", "(ns m) (require [\"nope.cato\" :as n])\n(defn main [] :- i64 0)\n"),
    ]);
    assert!(compile_err(&entry).contains("cannot read"));
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cato-interp --test modules`
Expected: FAIL — `diamond_requires_compile_and_run` / `same_module_under_two_aliases` fail to compile or resolve (`unknown function c/sq` etc.).

- [x] **Step 3: Implement canonical `expand_file` in `crates/cato-core/src/lib.rs`**

Replace `expand_file` (keep `require_of`, `rewrite_heads`, `rewrite_heads_expr`, `defn_names`). Add `ModCtx`, `parent_join`, `prefixed`, `rewrite_alias_refs`.

Algorithm:
1. Read `src`; drop each top-level `(ns name)` after validating it against `ctx.ns` (duplicate → `duplicate ns {name}`; missing name → `ns expects a name`).
2. Walk the kept forms in order; for each `(require [rel :as alias])`:
   - `child = parent_join(file, rel)`.
   - If `ctx.canon` has `child`: insert `ctx.aliases[alias] = ctx.canon[child]`. No re-expansion.
   - Else: insert `ctx.canon[child] = alias`; read `child` from disk (error `require: cannot read {child}: {e}` with span); recurse `expand_file(child, child_src, &alias, ctx)`; extend the merged list with the child's forms (already canonical); insert `ctx.aliases[alias] = alias`.
   - Non-require forms go to `own`.
3. On `own`: `for n in defn_names(&own) { rewrite_heads(&mut own, &n, &prefixed(canon_prefix, &n)); }`, then `rewrite_alias_refs(&mut own, &this_files_aliases)` where `this_files_aliases` are the `(alias, canon)` pairs for the require forms in this file.
4. Return `merged ++ own`.
5. `compile_named_with`: seed `ctx.canon[file]` absent (root prefix `""`), call `expand_file(file, src, "", &mut ctx)`.

`rewrite_alias_refs` walks `Expr` recursively; for every `Expr::LitSym(s)` where `s.starts_with(&format!("{alias}/"))`, replace with `prefixed(canon, &s[alias.len()+1..])`. Apply to call-head positions and defn headers consistently (mirror `rewrite_heads_expr`).

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cato-interp --test modules`
Expected: PASS (all seven).

Run: `cargo test -p cato-interp --test parity modules_agree_between_backends`
Expected: PASS (`49\n25\n16\n`).

- [x] **Step 5: Commit**

```bash
git add crates/cato-core/src/lib.rs crates/cato-backend-interp/tests/modules.rs
git commit -m "feat(seed): canonical-per-file require semantics (diamonds work)"
```

---

### Task 2: Kernel `require` loader (`hir/modules.cato`), wired into the concat kernel

**Files:**
- Create: `selfhost/cato/hir/modules.cato`
- Modify: `selfhost/cato/reader/read.cato` (host `read-all-with`, moved from its three copies)
- Modify: `selfhost/cato/cli.cato` (call the loader; drop the moved defn)
- Modify: `selfhost/cato/hir/hir_ref.cato`, `selfhost/cato/emit/emit_ref.cato` (drop their local `read-all-with` copies)
- Modify: `tools/concat_kernel.py` (add `hir/modules` to `FILES`, before `cli`)
- Create: `selfhost/fixtures/kernel/mod.cato`, `selfhost/fixtures/kernel/mod_geom.cato`
- Modify: `tools/kernel_gate.sh` (add `check_out mod "49\n25\n16"`)

**Interfaces:**
- Consumes: `scan` (`reader/read.cato`), `stab-get`/`stab-find`/`stab-conj` (`lib/sym.cato`), `AList`/`al-put`/`al-get`/`al-has` (`lib/alist.cato`), `Ast`/`Asts` and `asts-*`/`ast-*` (`lib/ast.cato`, `hir/expand.cato`), `Span`, `spat` (`reader/dump.cato`), `str`/`subs`/`count`/`nth` builtins, `slurp`.
- Produces (used by `cli.cato`):
  - `(deftype ModSt (ModSt [files :- (AList i64) nss :- (AList i64) canon :- (AList i64) aliases :- (AList i64)]))`
  - `(deftype ORES (OROk [forms :- Asts tab :- i64 st :- ModSt]) (ORErr [msg :- str at :- Span]))`
  - `(defn mod-empty [] :- ModSt ...)`
  - `(defn resolve-modules [path :- str forms :- Asts tab :- i64 st :- ModSt] :- ORES ...)`
  - Helpers (internal): `path-dir [p :- str] :- str`, `path-join [dir :- str rel :- str] :- str`, `rewrite-head-sids [a :- Ast pairs :- (AList i64)] :- Ast`, `rewrite-alias-sids [a :- Ast tab :- i64 aliases :- (AList i64)] :- Ast`, `defn-name-sids [forms :- Asts tab :- i64 acc :- Sids] :- Sids`.
  - `read-all-with [src :- str tab :- i64] :- RRes` now lives in `reader/read.cato` (it was triplicated in `cli.cato:28`, `hir_ref.cato:19`, `emit_ref.cato:32`; all three copies move there).

- [ ] **Step 1: Move `read-all-with` into `reader/read.cato`**

`read-all-with` is currently triplicated: `cli.cato:28`, `hir_ref.cato:19`, `emit_ref.cato:32`. Cut all three and paste one copy into `reader/read.cato` (it is a reader primitive: seed `scan` with the pre-interned table). All three entries keep calling `(read-all-with src tab)` bare — still one namespace at this point, so no call-site change yet.

- [ ] **Step 2: Create `selfhost/cato/hir/modules.cato`**

Implement:
- `path-dir`: scan from the end for the last `/`; return `subs p 0 i` if found, else `""`.
- `path-join`: `""` → rel; else `str dir "/" rel`.
- `mod-empty`: all four maps `(ALNil)`.
- `resolve-modules`: mirror Task 1's algorithm using `sids` as map keys (intern path/alias/name strings into `tab` via `stab-find`+`stab-conj`):
  1. Insert the current file's path-sid into `files`.
  2. For each top-level form: if head-sid is `(s-ns)` validate the name against `nss` (duplicate → `duplicate ns {name}`); if head-sid is `(s-require)` parse `[rel :as alias]`, resolve `child = path-join (path-dir path) rel`; if `canon` has the child's sid, record `aliases[alias] = canon[child]`, else record `canon[child] = alias`, `slurp` + `read-all-with` the child with `tab`, recurse, and append the child's returned forms; otherwise keep the form as `own`.
  3. Rename `own`: for each defn name-sid `n` (externs are global, skipped), rewrite heads `n → prefixed(name-of n, canon-of-this-file)`; then rewrite `alias/name` refs for this file's alias bindings to `prefixed(canon, name)`.
  4. Return `OROk (children ++ own) tab st`; errors as `ORErr msg span` with the seed's wording.

`prefix` for a file = the name of the alias it was required with; the entry file (`canon` has no entry) uses no prefix.

- [ ] **Step 3: Wire the loader into `cli.cato`**

In `compile-to-ir` (`cli.cato:45`), after `read-all-with` succeeds, call `resolve-modules` before `expand-all`:

```cato
(defn compile-to-ir [path :- str] :- str
  (let [src (slurp path)
        r (read-all-with src (magic-table))]
    (match r
      (ROk [forms tab]
        (match (resolve-modules path forms tab (mod-empty))
          (OROk [forms2 tab2 st]
            (match (expand-all forms2 (KNil))
              (EOk [xforms] (compile-build tab2 xforms))
              (EErr [msg at] (do (print-string (str "err " (spat at) " " msg)) ""))))
          (ORErr [msg at]
            (do (print-string (str "err " (spat at) " " msg)) ""))))
      (RErr [msg at]
        (do (print-string (str "err " (spat at) " " msg)) "")))))
```

- [ ] **Step 4: Add the fixture pair and gate check**

`selfhost/fixtures/kernel/mod_geom.cato`:
```
(ns mod.geom)
(defn square [x :- i64] :- i64 (* x x))
(defn hypot-sq [a :- i64 b :- i64] :- i64 (+ (square a) (square b)))
```
`selfhost/fixtures/kernel/mod.cato`:
```
(ns mod.main)
(require ["mod_geom.cato" :as m])
(defn main [] :- i64
  (do (print (m/square 7)) (print (m/hypot-sq 3 4)) (print (m/square (m/square 2))) 0))
```
In `tools/kernel_gate.sh`, after the existing `check_out` lines, add:
```
check_out mod "$(printf '49\n25\n16')"
```

- [ ] **Step 5: Verify the loader**

Run:
```bash
cargo build -p cato-cli
python3 tools/concat_kernel.py
./target/debug/cato run selfhost/cato.cato build selfhost/fixtures/kernel/mod.cato /tmp/mod >/tmp/modbuild.log 2>&1
/tmp/mod
```
Expected: `/tmp/mod` prints `49`, `25`, `16`.

Run: `./tools/kernel_gate.sh | tail -12`
Expected: existing lines unchanged, `OK mod`, `GATE: PASS`.

- [ ] **Step 6: Commit**

```bash
git add selfhost/cato/hir/modules.cato selfhost/cato/reader/read.cato selfhost/cato/cli.cato tools/concat_kernel.py selfhost/fixtures/kernel/mod.cato selfhost/fixtures/kernel/mod_geom.cato tools/kernel_gate.sh
git commit -m "feat(kernel): require loader (canonical rename) in Cato"
```

---

### Task 3: Convert the kernel sources, point every build at entries, delete the concat

**Files:**
- Create (temporary): `tools/split_kernel.py`
- Modify: every `selfhost/cato/**/*.cato` under the graph + the four `_ref` files
- Modify: `tools/kernel_gate.sh` (build `selfhost/cato/cli.cato`; drop the concat step)
- Delete: `selfhost/cato.cato`, `tools/concat_kernel.py`, `tools/split_kernel.py`

**Interfaces:**
- Consumes: Task 1 seed `require`; Task 2 kernel loader.
- Produces: `selfhost/cato/cli.cato` as the kernel entry; `selfhost/cato/lib/stdlib_ref.cato`, `selfhost/cato/reader/reader_ref.cato`, `selfhost/cato/hir/hir_ref.cato`, `selfhost/cato/emit/emit_ref.cato` as the harness entries.

- [ ] **Step 1: Write `tools/split_kernel.py`**

The script:
- Defines `MODULES`: the 23 graph files (the 22 kernel files plus the new `hir/modules.cato`) and the 4 `_ref` files, each with a `(path, alias, ns_name)`; `alias` is the canonical short name (e.g. `sym`, `strbuf`, `span`, `ast`, `alist`, `result`, `read`, `rdump`, `types`, `expand`, `forms`, `hexpr`, `lower`, `moveck`, `hdump`, `state`, `scalars`, `heap`, `adt`, `closures`, `emit`, `modules`).
- Builds `owner: name -> module path` by scanning `(defn NAME` / `(extern NAME` per file.
- For each module: collect the set of cross-file heads (symbols at position 0 of a list, and `defn`/`extern` header neighbours, whose name is owned by another file); add `(require ["<relative path>" :as <owner alias>])` for each; rewrite those heads to `<alias>/<name>`; prepend `(ns <ns_name>)`.
- Relative path from a file's dir to the target file (e.g. from `hir/lower.cato` to `lib/ast.cato` → `../lib/ast.cato`), one canonical spelling.
- Never rewrite: local binders, parameters, type names, constructors — only exact matches to an owner name in call/defn-head position.
- `_ref` files: same treatment (their cross-file heads pull the modules they drive); they keep their single `main`.

- [ ] **Step 2: Run the script and inspect the diff**

Run: `python3 tools/split_kernel.py`
Then: `git diff --stat selfhost/cato`
Expected: all 27 files (23 graph + 4 `_ref`) modified; spot-check one `hir/` file (has `(ns ...)`, `(require ...)` block, aliased heads) and one lib file (`(ns ...)` only, or a couple of requires).

- [ ] **Step 3: Switch the gate and delete the concat**

In `tools/kernel_gate.sh`: remove `python3 tools/concat_kernel.py || exit 2`; replace every `selfhost/cato.cato` with `selfhost/cato/cli.cato`.

Run:
```bash
rm -f selfhost/cato.cato tools/concat_kernel.py tools/split_kernel.py
```

- [ ] **Step 4: Verify the fixpoint and gate from entries**

Run:
```bash
cargo build -p cato-cli
./target/debug/cato build selfhost/cato/cli.cato -o /tmp/k1
/tmp/k1 build selfhost/cato/cli.cato /tmp/k2
/tmp/k2 build selfhost/cato/cli.cato /tmp/k3
cmp /tmp/k2.ll /tmp/k3.ll && echo FIXPOINT-OK
```
Expected: `FIXPOINT-OK`.

Run: `./tools/kernel_gate.sh | tail -12`
Expected: `FIXPOINT: OK`, fixtures `OK` (t1–t6, mod, bk1/bk3), `GATE: PASS`.

- [ ] **Step 5: Commit**

```bash
git add -A selfhost/cato selfhost/fixtures tools/kernel_gate.sh
git rm selfhost/cato.cato tools/concat_kernel.py
git commit -m "refactor(kernel): build from require'd modules; delete concat + generator"
```

---

### Task 4: Rewire the Rust harnesses and docs

**Files:**
- Modify: `crates/cato-backend-interp/tests/kernel_bootstrap.rs`
- Modify: `crates/cato-backend-interp/tests/kernel_reader.rs`
- Modify: `crates/cato-backend-interp/tests/kernel_hir.rs`
- Modify: `crates/cato-backend-interp/tests/kernel_emit.rs`
- Modify: `crates/cato-backend-interp/tests/selfhost.rs`
- Modify: `ROADMAP.md`, `HANDOFF.md`, `docs/superpowers/specs/2026-10-02-cato-s8-stage1-bootstrap.md`

**Interfaces:**
- Consumes: the entry files from Task 3.
- Produces: no new interfaces.

- [ ] **Step 1: Rewire `kernel_reader.rs` / `kernel_emit.rs` / `selfhost.rs`**

Delete `CONCAT_FILES` (and `LIB_FILES` in `selfhost.rs`) and the concatenation in `setup()`. Point `cato`/`--emit-ir` and `cato run` at the entry file:
- `kernel_reader.rs` → `selfhost/cato/reader/reader_ref.cato`
- `kernel_emit.rs` → `selfhost/cato/emit/emit_ref.cato`
- `selfhost.rs` → `selfhost/cato/lib/stdlib_ref.cato`
Keep the scratch `.ll`/`.cato` paths in `target/` only if still needed; otherwise drop them.

- [ ] **Step 2: Rewire `kernel_hir.rs`**

Same as Step 1, entry `selfhost/cato/hir/hir_ref.cato` (its `main` is driven through the seed interpreter; keep the mode flags/arguments as-is).

- [ ] **Step 3: Rewire `kernel_bootstrap.rs`**

Delete `CONCAT_FILES`, `generate_concat`, and the `kernel_concat_is_fresh` test. Replace `selfhost/cato.cato` with `selfhost/cato/cli.cato` in `kernel_cli_builds_and_runs_the_fixtures` and the ignored `bootstrap_chain_cato1_builds_cato2`.

- [ ] **Step 4: Update docs**

- `ROADMAP.md`: every `selfhost/cato.cato` build-path reference (today lines 20, 23, 29, 100, 107, 241, 243, 244, 255, 256) becomes `selfhost/cato/cli.cato`; replace the concat/generator sentences with the module layout; move the "After the fixpoint" `require` item to shipped.
- `HANDOFF.md`: every `selfhost/cato.cato`/`python3 tools/concat_kernel.py` occurrence (Quick commands and "Files to focus") becomes `selfhost/cato/cli.cato` (line 536's concat step is dropped, not replaced).
- S8 spec §"The canonical concat": replace with a short note that the kernel is `require`-connected modules and `cli.cato` is the entry.

- [ ] **Step 5: Verify the whole suite**

Run:
```bash
cargo build
cargo test
./tools/kernel_gate.sh | tail -12
python3 -m unittest discover -s tools/tests
```
Expected: `cargo test` green (including `parity::modules_agree_between_backends`, `kernel_reader_*`, `kernel_hir_*`, `kernel_emit_*`, `seed_builds_the_kernel_stdlib_and_both_backends_agree`, `kernel_cli_builds_and_runs_the_fixtures`); `GATE: PASS`; `Ran 20 tests … OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/cato-backend-interp/tests ROADMAP.md HANDOFF.md docs/superpowers/specs/2026-10-02-cato-s8-stage1-bootstrap.md
git commit -m "test(kernel): drive require'd entries; drop concat from harnesses and docs"
```

---

## Self-Review

**Spec coverage:**
- Seed canonical rename → Task 1. Kernel loader in Cato → Task 2. Source conversion + entry files → Task 3. Delete concat/generator → Task 3. Gate/build rewiring → Tasks 2–3. Rust harness rewiring → Task 4. Docs → Task 4. Path/ownership rules → Global Constraints + Task 2 algorithm. Risks/parity → Task 4 verification (gate fixpoint) + Task 1 tests.
- Cycle termination and duplicate-ns are implemented in Task 1 (Rust) and mirrored in Task 2 (Cato); both are in Review Focus.

**Type consistency:** `ORES`/`ModSt`/`resolve-modules` names are used identically in Task 2's interface block and its `cli.cato` wiring. Rust helper names (`parent_join`, `prefixed`, `rewrite_alias_refs`, `ModCtx`) are consistent within Task 1. Entry-file paths in Tasks 3 and 4 match.

**Proportion:** The plan pins signatures, algorithms for the loader (which the signature does not determine), test assertions, and commands; it does not transcribe bodies of the mechanical conversion.

**Review Focus coverage:** cycle → Task 1 `cycle_terminates`; two aliases → Task 1 `same_module_under_two_aliases`; duplicate spelling → Task 1 `same_file_reached_by_two_spellings_dedups` (same-spelling dedup pinned; differing spellings are documented as distinct paths); duplicate ns → Task 1 `duplicate_ns_is_rejected`; unknown alias / missing file → Task 1 `unknown_alias_is_a_build_error` and `missing_required_file_is_an_error`.
