# Kernel Clone-Elimination Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the self-hosting Cato kernel's compile-time peak RSS (currently ~8.1 GB; seed baseline 0.22 GB) by deleting the redundant source-level `(clone …)` calls the kernel carries, without changing generated code.

**Architecture:** The kernel is a Rust port whose ownership model was approximated with clone-at-every-use. Those clones are written, not forced — the seed's ownership checker accepts moves where the value is used once. We build a scope-aware source transform (`tools/unclone.py`) on the parser and scope machinery already in `tools/autoclone.py`, apply it file by file, and gate every change on a byte-identical fixpoint plus a runtime fixture suite.

**Tech Stack:** Cato (self-hosted compiler), Python 3 (codemod), LLVM IR via the LLVM backend, bash (gate script), macOS `/usr/bin/time -l` for RSS.

**Spec:** `docs/superpowers/specs/2026-10-16-kernel-clone-elimination-design.md`

## Global Constraints

- Hand-write nothing in `selfhost/cato.cato` — it is generated; regenerate with `python3 tools/concat_kernel.py` before every build.
- All kernel builds during development use the seed: `./target/debug/cato` (rebuild with `cargo build -p cato` if stale).
- Correctness gate after **every** source change: `cmp /tmp/k2.ll /tmp/k3.ll` must report no differences (byte-identical fixpoint).
- Fixture behavior is fixed and must not change: `t1..t6` print `la=X`, `X`, `7`, `3`, `3`, `3`; `bk1`, `bk3`, `bk4` are accepted; `bk2` is rejected (nonzero exit).
- Never introduce `Map`/`Set` (the codegen forbids them as params/returns, `crates/cato-backend-llvm/src/llvm/funcs.rs:266,308`; the kernel's own compiler rejects map/set literals).
- Peak RSS is measured with `/usr/bin/time -l`, field `maximum resident set size` (bytes on macOS).
- One commit per task; each commit message states the measured RSS and the fixpoint result.

## Review Focus

Five failure modes this plan's small fixtures do **not** by themselves cover; each is pinned to a task in the list that follows.

1. **Use-after-move / double free.** Removing a clone that was load-bearing must be rejected by the ownership checker, not silently miscompiled. Pinned by Task 7 (ASAN + `bk2`).
2. **A clone removal that only misbehaves on larger inputs.** The t1–t6 fixtures are tiny. Pinned by Task 7 (compile+run the full `selfhost/cato/hir/fixtures/*.cato` suite under the rebuilt kernel).
3. **Codemod source corruption.** Strings/comments containing `clone`, or nested scopes that shadow a binder, could be mangled. Pinned by Task 2 unit tests and a `git diff` review step in every apply task.
4. **Fixpoint flakiness.** If codegen embeds pointer values, `k2.ll == k3.ll` could pass for the wrong reason or flake. Pinned by Task 7 (run the fixpoint twice, confirm stable).
5. **RSS regression on ordinary programs.** The compiler is used on small inputs too; a change that helps the kernel could hurt the common case. Pinned by Task 7 (measure RSS on a small program, compare to Task 1 baseline).

---

## File Structure

- `tools/unclone.py` — **new.** Clone-elision source transform: parses Cato (reusing `tools/autoclone.py`'s tokenizer, parser and scope logic), and removes `(clone X)` where `X` is used once in its binding scope. Reads a file, writes the transformed text to stdout. One responsibility: source → source.
- `tools/tests/test_unclone.py` — **new.** Self-contained `unittest` module pinning the transform's behavior with exact input/output pairs.
- `tools/kernel_gate.sh` — **new.** Reproducible correctness+RSS gate: concat, seed build, fixpoint, fixtures, RSS. One responsibility: run the gate.
- `selfhost/fixtures/kernel/{t1..t6,bk1..bk4}.cato` — **new.** Durable copies of the scratch fixtures so the gate is reviewable and self-contained.
- `selfhost/cato/hir/moveck.cato`, `selfhost/cato/hir/lower.cato`, `selfhost/cato/hir/forms.cato`, `selfhost/cato/cli.cato`, `selfhost/cato/emit/*.cato` — **modify** (by running `unclone.py`). Not hand-edited.
- `docs/superpowers/plans/2026-10-16-kernel-clone-elimination.md` — this plan.

---

## Task 1: Durable fixtures + reproducible gate harness + baseline

**Files:**
- Create: `selfhost/fixtures/kernel/t1.cato` … `t6.cato`, `selfhost/fixtures/kernel/bk1.cato` … `bk4.cato`
- Create: `tools/kernel_gate.sh`

**Interfaces:**
- Consumes: nothing.
- Produces: `tools/kernel_gate.sh` (no args) — runs concat + fixpoint + fixtures, prints `FIXPOINT: OK|DIFF` and `MAXRSS_BYTES: <n>`, exits nonzero on any failure. Every later task runs it.

- [ ] **Step 1: Create the fixture files with exact contents**

`selfhost/fixtures/kernel/t1.cato`:
```clojure
(defn f [l :- str] :- str (str "la=" l "\n"))
(defn main [] :- i32 (do (print-string (f "X")) 0))
```
`t2.cato`:
```clojure
(defn f [l :- str] :- str (str l "\n"))
(defn main [] :- i32 (do (print-string (f "X")) 0))
```
`t3.cato`:
```clojure
(defn f [n :- i64] :- str (str n))
(defn main [] :- i32 (do (print-string (f 7)) 0))
```
`t4.cato`:
```clojure
(defn f [s :- str] :- str (str (count s)))
(defn main [] :- i32 (do (print-string (f "la=")) 0))
```
`t5.cato`:
```clojure
(deftype HEX (HLit [v :- str]) (HOther [n :- str]))
(deftype E (E [p :- str l :- str]))
(defn go [e :- HEX] :- str
  (match e
    (HLit [v0] "lit")
    (_ "other")))
(defn f [e :- HEX] :- E
  (let [r (go (clone e))]
    (match e
      (HLit [v] (E "p" (str (count v))))
      (_ (E "p" "0")))))
(defn main [] :- i32
  (do (print-string (match (f (HLit "la=")) (E [p l] l))) 0))
```
`t6.cato`:
```clojure
(deftype HEX (HLit [v :- str]) (HOther [n :- str]))
(deftype E (E [p :- str l :- str]))
(defn f [e :- HEX] :- E
  (match e
    (HLit [v] (E "p" (str (count v))))
    (_ (E "p" "0"))))
(defn main [] :- i32
  (do (print-string (match (f (HLit "la=")) (E [p l] l))) 0))
```
`bk1.cato`:
```clojure
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
```
`bk2.cato`:
```clojure
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn bad [n :- &N] :- &N (Node n n))
(defn main [] :- i64 0)
```
`bk3.cato`:
```clojure
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
```
`bk4.cato`:
```clojure
(deftype Box (B [v :- i64]))
(defn f [x :- &Box] :- &Box x)
(defn main [] :- i64 0)
```

- [ ] **Step 2: Create `tools/kernel_gate.sh`**

```bash
#!/usr/bin/env bash
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SEED=./target/debug/cato
FX=selfhost/fixtures/kernel
fail=0

[ -x "$SEED" ] || { echo "seed missing: cargo build -p cato"; exit 2; }

python3 tools/concat_kernel.py || exit 2

"$SEED" build selfhost/cato.cato -o /tmp/k1 || exit 2
/tmp/k1 build selfhost/cato.cato -o /tmp/k2 || exit 2
/tmp/k2 build selfhost/cato.cato -o /tmp/k3 || exit 2
if cmp -s /tmp/k2.ll /tmp/k3.ll; then echo "FIXPOINT: OK"; else echo "FIXPOINT: DIFF"; fail=1; fi

check_out() { # file expected
  /tmp/k1 build "$FX/$1.cato" -o /tmp/run_$1 >/dev/null 2>&1 || { echo "BUILD FAIL $1"; fail=1; return; }
  got="$(/tmp/run_$1)"; [ "$got" = "$2" ] || { echo "OUTPUT FAIL $1: '$got' != '$2'"; fail=1; }
}
check_out t1 "la=X"; check_out t2 "X"; check_out t3 "7"
check_out t4 "3";    check_out t5 "3"; check_out t6 "3"

check_ok()  { /tmp/k1 build "$FX/$1.cato" -o /tmp/run_$1 >/dev/null 2>&1 && echo "OK $1" || { echo "ACCEPT FAIL $1"; fail=1; }; }
check_bad() { /tmp/k1 build "$FX/$1.cato" -o /tmp/run_$1 >/dev/null 2>&1 && { echo "REJECT FAIL $1"; fail=1; } || echo "OK $1(rejected)"; }
check_ok bk1; check_bad bk2; check_ok bk3; check_ok bk4

RSS=$(/usr/bin/time -l /tmp/k1 build selfhost/cato.cato -o /tmp/k2 2>&1 >/dev/null | awk '/maximum resident set size/{print $1}')
echo "MAXRSS_BYTES: $RSS"
echo "GATE: $([ $fail -eq 0 ] && echo PASS || echo FAIL)"
exit $fail
```

- [ ] **Step 3: Run the gate and record the baseline**

Run: `chmod +x tools/kernel_gate.sh && ./tools/kernel_gate.sh`
Expected: `FIXPOINT: OK`, six fixture `OK`s, `bk2` shows `OK bk2(rejected)`, `GATE: PASS`, and a `MAXRSS_BYTES:` line. Record that number — it is the Task 1 baseline every later task compares against.

- [ ] **Step 4: Commit**

```bash
git add tools/kernel_gate.sh selfhost/fixtures/kernel
git commit -m "test(kernel): add reproducible clone-elimination gate + fixtures (baseline MAXRSS=<N>)"
```

---

## Task 2: `tools/unclone.py` clone-elision transform

Reuse `tools/autoclone.py`'s `scan`, `parse`, `collect_types`, `transform_defn`/`transform_arm` scope logic rather than writing a new parser. The transform is the inverse of autoclone: it removes a `(clone X)` when `X` is a scope-local binding whose **last** use in that scope is the clone site (earlier uses stay cloned; the last becomes the move).

**Files:**
- Create: `tools/unclone.py`
- Test: `tools/tests/test_unclone.py`

**Interfaces:**
- Consumes: `tools/autoclone.py` (import its `scan`/`parse`/`collect_types`/scope helpers, or vendor them).
- Produces: `python3 tools/unclone.py FILE.cato` writes transformed source to **stdout**. Also exposes `unclone_text(text: str, type_sources: list[str]) -> tuple[str, int]` returning `(new_text, removed_count)`, used by the tests.

- [ ] **Step 1: Write the failing tests**

Create `tools/tests/test_unclone.py`:
```python
import os, sys, unittest
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import unclone

def run(src):
    return unclone.unclone_text(src, [])[0]

class TestUnclone(unittest.TestCase):
    def test_single_use_becomes_move(self):
        self.assertEqual(
            run("(defn f [x :- Box] :- i64 (get (clone x)))"),
            "(defn f [x :- Box] :- i64 (get x))")

    def test_last_of_two_uses_moves_earlier_stays_cloned(self):
        self.assertEqual(
            run("(defn f [x :- Box] :- i64 (+ (size (clone x)) (size (clone x))))"),
            "(defn f [x :- Box] :- i64 (+ (size (clone x)) (size x)))")

    def test_match_arm_binders_move_per_arm(self):
        src = ("(defn f [e :- HExpr] :- HExpr\n"
               "  (match e\n"
               "    (HLit [v] (HLit (clone v)))\n"
               "    (HOther [n] (HOther (clone n)))))")
        out = run(src)
        self.assertIn("(HLit v)", out)
        self.assertIn("(HOther n)", out)

    def test_shadowed_inner_binding_not_counted(self):
        src = ("(defn f [x :- Box] :- i64\n"
               "  (let [y (size (clone x))]\n"
               "    (+ y (size (clone x)))))")
        out = run(src)
        self.assertIn("(size x)", out)          # last use of outer x moves
        self.assertIn("(size (clone x))", out)  # first use stays cloned

    def test_strings_and_comments_untouched(self):
        src = '(defn f [x :- Box] :- str\n  ; (clone x) in a comment\n  (get (str (clone x))))'
        out = run(src)
        self.assertIn("; (clone x) in a comment", out)

    def test_copy_types_left_alone(self):
        src = "(defn f [n :- i64] :- i64 (add (clone n) (clone n)))"
        self.assertEqual(run(src), src)

    def test_idempotent(self):
        src = "(defn f [x :- Box] :- i64 (get (clone x)))"
        self.assertEqual(run(run(src)), run(src))

if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python3 -m unittest tools.tests.test_unclone -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'unclone'`.

- [ ] **Step 3: Implement `tools/unclone.py`**

Reuse `tools/autoclone.py`'s `NONCOPY_TYPES`, `TOK`, `scan`, `parse`, `collect_types`, and the scope-descending structure of `transform_defn`/`transform_arm`/`let`. Algorithm per scope, for each in-scope non-Copy binder `X`:

1. Walk the scope body, skipping subtrees that rebind `X` (lexical shadowing), and record every occurrence of `X`: both bare atoms and the argument position of `(clone X)`. Do **not** descend into nested `defn`s.
2. If total uses `n == 0`, nothing to do.
3. Order uses by source position; the **last** use is the move:
   - last use wrapped in `(clone X)` → record an edit to replace that form with `X` (unwrap);
   - last use bare → leave.
   - every earlier use bare → record an edit to wrap it as `(clone X)` (defensive; autoclone output already has them).
4. Apply edits right-to-left by source offset (as `autoclone.main` does) so offsets stay valid.

Top-level `unclone_text` parses, collects types from `type_sources` plus the file, runs the scope walk over every `defn`, applies edits, returns `(text, removed_count)`. `__main__` reads `sys.argv[1]`, uses `sys.argv[2:]` as type sources, writes the result to stdout.

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m unittest tools.tests.test_unclone -v`
Expected: PASS (all 7).

- [ ] **Step 5: Sanity-check against a real kernel file (no write)**

Run: `python3 tools/unclone.py selfhost/cato/hir/moveck.cato > /tmp/moveck.uncloned.cato && diff <(grep -c '(clone ' selfhost/cato/hir/moveck.cato) <(grep -c '(clone ' /tmp/moveck.uncloned.cato) ; grep -c '(clone ' /tmp/moveck.uncloned.cato`
Expected: the clone count is strictly lower than `selfhost/cato/hir/moveck.cato`'s. If it is not, the scope walk is not matching the kernel's formatting — stop and fix before committing.

- [ ] **Step 6: Commit**

```bash
git add tools/unclone.py tools/tests/test_unclone.py
git commit -m "feat(tools): scope-aware clone-elision transform (unclone.py)"
```

---

## Task 3: Apply unclone to `moveck.cato` (spike) + gate + measure

**Files:**
- Modify (by running the tool): `selfhost/cato/hir/moveck.cato`
- Modify (generated): `selfhost/cato.cato`

**Interfaces:**
- Consumes: `tools/unclone.py` (Task 2), `tools/kernel_gate.sh` (Task 1).
- Produces: the validated elision workflow (transform → concat → gate → inspect diff → commit) reused verbatim by Tasks 4–5; a measured RSS delta proving the approach.

- [ ] **Step 1: Transform and inspect the diff before applying**

Run: `python3 tools/unclone.py selfhost/cato/hir/moveck.cato > /tmp/moveck.new.cato && diff selfhost/cato/hir/moveck.cato /tmp/moveck.new.cato | head -60`
Expected: only `(clone X)` → `X` unwraps; no brace/paren imbalance, no change inside strings or comments. Confirm by eye, then: `cp /tmp/moveck.new.cato selfhost/cato/hir/moveck.cato`.

- [ ] **Step 2: Run the gate**

Run: `./tools/kernel_gate.sh`
Expected: `FIXPOINT: OK`, all fixtures `OK`, `GATE: PASS`.
If it reports `FIXPOINT: DIFF` or a compile error: the transform removed a clone still needed. The error names file:line; restore `(clone …)` at the offending site, and re-run. Repeat until `GATE: PASS`.

- [ ] **Step 3: Confirm the RSS dropped**

Compare `MAXRSS_BYTES` to the Task 1 baseline.
Expected: strictly lower. If it is **not** lower, stop the plan and report: the hypothesis (source clones dominate peak RSS) is wrong for this phase and the plan must be revisited before Tasks 4–6.

- [ ] **Step 4: Commit**

```bash
git add selfhost/cato/hir/moveck.cato selfhost/cato.cato
git commit -m "perf(kernel): elide redundant clones in moveck (MAXRSS=<N>, fixpoint OK)"
```

---

## Task 4: Apply unclone to `lower.cato` + gate + measure

**Files:**
- Modify (by running the tool): `selfhost/cato/hir/lower.cato`
- Modify (generated): `selfhost/cato.cato`

**Interfaces:**
- Consumes: `tools/unclone.py`, `tools/kernel_gate.sh`, the workflow from Task 3.
- Produces: reduced RSS from the largest clone site (`lower.cato`, 2397 clones).

- [ ] **Step 1: Transform and inspect**

Run: `python3 tools/unclone.py selfhost/cato/hir/lower.cato > /tmp/lower.new.cato && diff selfhost/cato/hir/lower.cato /tmp/lower.new.cato | head -60`
Expected: only `(clone X)` → `X` unwraps. Then `cp /tmp/lower.new.cato selfhost/cato/hir/lower.cato`.

- [ ] **Step 2: Run the gate; fix residual ownership errors**

Run: `./tools/kernel_gate.sh`
Expected: `GATE: PASS`. If the build reports use-after-move at file:line, restore the clone there and re-run. Repeat until green.

- [ ] **Step 3: Record RSS and commit**

Compare `MAXRSS_BYTES` to Task 3.
```bash
git add selfhost/cato/hir/lower.cato selfhost/cato.cato
git commit -m "perf(kernel): elide redundant clones in lower (MAXRSS=<N>, fixpoint OK)"
```

---

## Task 5: Apply unclone to `forms.cato`, `cli.cato`, and `emit/*.cato` + gate + measure

**Files:**
- Modify (by running the tool): `selfhost/cato/hir/forms.cato`, `selfhost/cato/cli.cato`, `selfhost/cato/emit/scalars.cato`, `selfhost/cato/emit/heap.cato`, `selfhost/cato/emit/adt.cato`, `selfhost/cato/emit/closures.cato`, `selfhost/cato/emit/emit.cato`
- Modify (generated): `selfhost/cato.cato`

**Interfaces:**
- Consumes: `tools/unclone.py`, `tools/kernel_gate.sh`.
- Produces: elision of the remaining source clone sites.

- [ ] **Step 1: Transform each file and inspect**

Run for each file `F` in the list above:
`python3 tools/unclone.py selfhost/cato/$F.cato > /tmp/F.new && diff selfhost/cato/$F.cato /tmp/F.new | head -40`
Expected: only `(clone X)` → `X`. Then copy each in:
`cp /tmp/forms.new selfhost/cato/hir/forms.cato` (etc.), or write a one-liner that loops the list and applies in place.

- [ ] **Step 2: Run the gate; fix residual ownership errors**

Run: `./tools/kernel_gate.sh`
Expected: `GATE: PASS`. Restore clones at any file:line the checker rejects; repeat until green.

- [ ] **Step 3: Record RSS and commit**

Compare `MAXRSS_BYTES` to Task 4.
```bash
git add selfhost/cato selfhost/cato.cato
git commit -m "perf(kernel): elide redundant clones in forms/cli/emit (MAXRSS=<N>, fixpoint OK)"
```

---

## Task 6: Borrow-aware elision (drop clones at `&T` parameters) — only if still large

**Files:**
- Modify: `tools/unclone.py`
- Test: `tools/tests/test_unclone.py`
- Modify (by running the tool): the same kernel files as Tasks 3–5.

**Interfaces:**
- Consumes: `tools/unclone.py`, `collect_types` from `tools/autoclone.py`.
- Produces: an extra rule — a `(clone X)` passed to a parameter declared `&T` is a borrow, never a move, and can always be dropped.

- [ ] **Step 1: Decide whether this task is needed**

Look at the Task 5 `MAXRSS_BYTES`. If it is already within ~2x of the seed's 0.22 GB, **skip this task** and go to Task 7. Otherwise continue — the remaining clones are dominated by multi-use binders passed to `&T` accessors (`hv-a`, `arms-of`, `hargs-of`), which rule 3 above cannot remove.

- [ ] **Step 2: Write the failing tests**

Add to `tools/tests/test_unclone.py`:
```python
    def test_clone_dropped_at_borrow_param(self):
        ts = ["(defn hv-a [e :- &HExpr] :- HExpr (hargs-of e))\n"
              "(defn g [x :- HExpr] :- HExpr (hv-a (clone x)))"]
        out = unclone.unclone_text(ts[1], ts)[0]
        self.assertEqual(out, "(defn g [x :- HExpr] :- HExpr (hv-a x))")

    def test_clone_kept_at_owned_param(self):
        ts = ["(defn take [e :- HExpr] :- HExpr e)\n"
              "(defn g [x :- HExpr] :- HExpr (take (clone x)))"]
        out = unclone.unclone_text(ts[1], ts)[0]
        self.assertEqual(out, "(defn g [x :- HExpr] :- HExpr (take (clone x)))")
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `python3 -m unittest tools.tests.test_unclone -v`
Expected: FAIL on the two new tests.

- [ ] **Step 4: Implement the borrow rule**

In `collect_types`, also record, per defn, the set of parameter **positions** whose declared type is `&T`. In the walk, when a `(clone X)` is the argument at a borrow position of a call whose head is a known defn, record an unwrap edit for it regardless of use count (it does not consume `X`, so `X` may still move elsewhere). Keep the rule 3 "last use" logic for all other sites.

- [ ] **Step 5: Run tests to verify they pass**

Run: `python3 -m unittest tools.tests/test_unclone -v`
Expected: PASS (all 9).

- [ ] **Step 6: Apply, gate, measure, commit**

Run over the same files as Tasks 3–5, then:
```bash
./tools/kernel_gate.sh
git add tools/unclone.py tools/tests/test_unclone.py selfhost/cato selfhost/cato.cato
git commit -m "perf(kernel): drop clones at borrow params (MAXRSS=<N>, fixpoint OK)"
```

---

## Task 7: Whole-chain acceptance + final report

**Files:**
- Modify: none (verification only). Optional: add a `docs/superpowers/plans/2026-10-16-kernel-clone-elimination.md` results footnote.

**Interfaces:**
- Consumes: everything above.
- Produces: the evidence that the refactor is correct and the RSS story is captured.

- [ ] **Step 1: Full gate, run twice, confirm stable**

Run: `./tools/kernel_gate.sh && ./tools/kernel_gate.sh`
Expected: `GATE: PASS` both times, identical `MAXRSS_BYTES` within noise, `FIXPOINT: OK` both times (pins Review Focus #4).

- [ ] **Step 2: ASAN over the rebuilt kernel compiling the kernel**

Run: `cargo build -p runtime --features asan 2>/dev/null || true` then rebuild the kernel binary with ASAN as the project's ASAN flow documents; run `/tmp/k1`-equivalent under ASAN on `selfhost/cato.cato`.
Expected: clean run, no double-free / use-after-free reports. If the project has no ASAN flow for the kernel, instead run the fixtures under ASAN-built binaries; record which was used. (Pins Review Focus #1.)

- [ ] **Step 3: Run the full HIR fixture suite under the rebuilt kernel**

Run: for each `selfhost/cato/hir/fixtures/*.cato`, compile+run with `/tmp/k1`, comparing against the seed's output.
Expected: identical outputs (the `err_*` fixtures must fail identically). (Pins Review Focus #2.)

- [ ] **Step 4: Small-program RSS check**

Run: `/usr/bin/time -l /tmp/k1 build selfhost/fixtures/kernel/t6.cato -o /tmp/t6bin 2>&1 >/dev/null | awk '/maximum resident set size/{print $1}'`
Expected: not higher than the equivalent measurement at the Task 1 baseline commit (pins Review Focus #5). To get the baseline, `git stash`/`git checkout` the baseline commit or re-run against the pre-change tree.

- [ ] **Step 5: Cross-check seed vs kernel output on the kernel**

Run: `./target/debug/cato build selfhost/cato.cato -o /tmp/seedk` then compare `/tmp/seedk.ll` with `/tmp/k1.ll` and the printed diagnostics.
Expected: no new divergence introduced by this work relative to the pre-change tree (record any pre-existing divergence so it is not attributed here).

- [ ] **Step 6: Record final numbers and commit**

Append a `## Results` section to this plan with: baseline vs final `MAXRSS_BYTES` (kernel-of-kernel), and the per-task RSS table.
```bash
git add docs/superpowers/plans/2026-10-16-kernel-clone-elimination.md
git commit -m "docs(plan): record kernel clone-elimination results (MAXRSS <baseline> -> <final>)"
```

---

## Self-Review

- **Spec coverage:** Lever A (source clone deletion) → Tasks 2–5, 6; Lever B (table consumers borrow) → Task 6 borrow rule; correctness gate → Task 1 + every task; measurement → Tasks 1, 3–6, 7; the spec's "spike `bcheck` first" → Task 3 (whole-file moveck, `bcheck` included). Levers' ordering preserved. No spec section is unimplemented.
- **Placeholder scan:** every step names a file, a command, and an expected result; `<N>` appears only in commit messages, to be filled from the measured `MAXRSS_BYTES`.
- **Type consistency:** `unclone_text(text, type_sources) -> (str, int)` is used consistently in Tasks 2 and 6; `tools/kernel_gate.sh` prints `MAXRSS_BYTES:` used by Tasks 3–7; fixture names are consistent across Tasks 1, 3–5, 7.
- **Review Focus:** all five lines are pinned (Task 7 steps 1–5, Task 2 tests).
- **Proportion:** plan ≈ spec length; only the gate script and fixtures are given as full text (they are exact copy), the transform is given as an algorithm, not a body.
