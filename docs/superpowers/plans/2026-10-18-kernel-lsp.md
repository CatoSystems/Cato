# Kernel LSP: milestones A (diagnostics) → B (definition/hover) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `cato1 lsp` — a self-hosted JSON-RPC LSP server over stdio written in Cato: milestone A = incremental framing + document sync + diagnostics; milestone B (immediately after) = `textDocument/definition` + `textDocument/hover` at behavioral parity with the seed's LSP. Also (flagged scope addition, Task 4) fix the seed LSP's `read_to_end` framing so it responds to persistent-stdin editors.

**Architecture:** `lib/json.cato` (recursive `JVal` parse + escape/serialize), `lib/lsp.cato` (header/body framing on the plan-3 stdin externs, message dispatch, AList document store, diagnostics from plan 3's `lib/compile.cato` `CRes` pipeline), `cli.cato` flips its `lsp` stub to the real server. Harnesses pipe `Content-Length`-framed message batches into subprocesses (seed `cato lsp` and `cato-interp-run … lsp`) and parse the framed responses.

**Tech Stack:** Cato (kernel), `runtime.c` externs from plan 3, Rust test harnesses.

**Spec:** `docs/superpowers/specs/2026-10-18-cli-contract-design.md` §4 — the plan argues from it; executors read both. Seed reference implementation: `crates/cato-cli/src/lsp.rs`.

## Global Constraints

- **The kernel stream is sacred:** nothing but framed JSON goes to stdout. All kernel-side errors → stderr via `cato-stderr` (or silently ignored for malformed messages — never a partial frame).
- Framing is **incremental** (spec §4): `stdin-getline` header loop + `stdin-read-exact` body — the seed's `read_to_end` pattern must NOT be copied (it blocks until stdin closes).
- Byte accounting: `Content-Length` counts bytes; `count` on `str` is byte length — but note `stdin-getline`/`stdin-read-exact` return C-string-safe payloads (JSON text never contains raw NUL).
- Position conversion (research-verified): kernel `Span` = 1-based line, 0-based byte col, len → LSP `{line: line-1, character: col}` / end `{line: line-1, character: col+len}`; pipeline errors with `line 0` (HIR-lower/moveck, no span) → range `{0,0}–{0,0}`. Seed LSP precedent: `lsp.rs:267-292`.
- Milestone A capabilities: **only** `{"textDocumentSync":1}` (do not advertise definition/hover before they exist). Milestone B: add `"definitionProvider":true,"hoverProvider":true` — same JSON shape as seed `lsp.rs:342-352`.
- Milestone B definition/hover must match the seed's *actual* behavior — a textual in-document scan for lines whose `trim_start` starts `"(defn "` (`lsp.rs:432-479`) — not an idealized HIR lookup (spec §4).
- No third-party deps; historical spec/plan docs untouched.

## Review Focus

1. **Framing must not block on EOF before responding** (the seed's `read_to_end` bug must not be copied). Test: both servers get an interleaved test — write `initialize`, read the framed response from stdout **with stdin still open** (reader-thread + `recv_timeout`), then send `shutdown`/`exit`. Seed-side this is Task 4's new test (timeout-fails pre-fix); kernel-side it is Task 2's `responds_before_stdin_closes` (proves the incremental loop, spec §4).
2. **Diagnostic positions must survive the 1-based→0-based conversion with the right column basis** (kernel col is already 0-based; the seed's is 1-based). Test: Task 2 crafts an error at a known source position (reader error with span) and asserts exact `line`/`character` values; a parity check against the seed's diagnostic for the same file is asserted field-wise.
3. **Malformed frames must not corrupt the session or stdout.** Test: Task 2 feeds a frame with body `not-json` between two valid messages; asserts the valid messages still get responses and stdout contains only complete frames.
4. **Full-sync `didChange` must use the last content change and recompile in-process** (path from `uri`, `file://` stripped). Test: Task 2 opens an erroneous doc (diagnostics non-empty), changes it to valid (diagnostics `[]`) — all within one session.
5. **Definition/hover parity with the seed.** Test: Task 3 runs both servers on the same document and asserts the same `line`/hover text for identical request ids.

---

### Task 1: `lib/json.cato` — parser + stringifier

**Files:**
- Create: `selfhost/cato/lib/json.cato`
- Test: `crates/cato-backend-interp/tests/kernel_json.rs`

**Interfaces:**
- Consumes: kernel stdlib (`subs`, `nth`, `count`, `str`, `parse-i64`/`parse-f64`, `(Vector str)`), recursive-deftype idiom proven by `reader/read.cato`'s `Asts`.
- Produces (Task 2 relies on):

```cato
(ns cato.json)
(deftype JVal (JNull) (JTrue) (JFalse) (JInt [n :- i64]) (JNum [n :- f64])
              (JStr [s :- str]) (JArr [items :- JVals]) (JObj [keys :- (Vector str) vals :- JVals]))
(deftype JVals (JVNil) (JVCons [h :- JVal t :- JVals]))
(deftype JRes (JOk [v :- JVal]) (JErr [msg :- str]))
(defn parse [s :- str] :- JRes …)
(defn get-field [o :- JVal k :- str] :- JVal)          ; JNull if absent/not an object
(defn as-str [v :- JVal] :- str)                        ; "" if not JStr
(defn as-int [v :- JVal] :- i64)                        ; 0 if not JInt/JNum
(defn escape [s :- str] :- str)                         ; JSON string escaping (" \ \n \r \t, \u00XX for other < 0x20)
(defn str-of [v :- JVal] :- str)                        ; serializer (compact, no spaces)
(defn msg-of [id :- i64 result :- str] :- str)          ; {"jsonrpc":"2.0","id":N,"result":…} with result already JSON text
(defn msg-of-null [result :- str] :- str)               ; same but "id":null (initialize without an id, matching seed lsp.rs:338)
```

Parse scope: full JSON — objects, arrays, literals, numbers (no `.`/`e` → `JInt`, else `JNum`), strings with `\" \\ \/ \n \r \t \b \f` and `\uXXXX`; for `\uXXXX`, decode code points < 0x100 to UTF-8 bytes (hand-rolled 1–2 byte encoding), code points ≥ 0x100 → `JErr "unsupported \\u escape"` (LSP test traffic never needs them; realistic editor traffic uses raw UTF-8). **No trailing garbage** (`"{} x"` → `JErr`).

- [ ] **Step 1: Write failing tests (`kernel_json.rs`)**

Harness (same pattern as `kernel_cli.rs`): a fixture under `target/kernel_json/` taking the JSON text as one program arg, printing canonical results.

```rust
#[test] fn parses_flat_object_fields()      // {"method":"initialize","id":1} -> method=initialize id=1
#[test] fn parses_nested_params()           // {"params":{"textDocument":{"uri":"file:///a"}}} -> get-field chain works
#[test] fn parses_array_and_numbers()       // [1,2.5,-3] -> JInt/JNum distinctions (print tags)
#[test] fn string_escapes_roundtrip()       // parse then str-of of "\"a\\b\n\"" == compact re-escape; escape() of raw control byte -> \u0001
#[test] fn unterminated_string_is_error()   // JErr
#[test] fn trailing_garbage_is_error()      // "{} x" -> JErr
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-backend-interp kernel_json` → FAIL

- [ ] **Step 3: Implement `lib/json.cato`** — recursive-descent over `s` with an index (`loop`/`recur` or defn threading per kernel style); skipWs on space/tab/newline/`\r`.

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-backend-interp kernel_json` → PASS

- [ ] **Step 5: Commit**

```bash
git add selfhost/cato/lib/json.cato crates/cato-backend-interp/tests/kernel_json.rs
git commit -m "feat(kernel): lib/json.cato — JSON parse/get/escape/stringify for LSP"
```

---

### Task 2: `lib/lsp.cato` — framing, sync, diagnostics (milestone A)

**Files:**
- Create: `selfhost/cato/lib/lsp.cato`
- Modify: `selfhost/cato/cli.cato` (flip `lsp` stub → `(do (lsp/serve) 0)`)
- Test: `crates/cato-backend-interp/tests/kernel_lsp.rs` (created here, extended in Task 3)

**Interfaces:**
- Consumes: plan 3 externs (`stdin-getline`, `stdin-read-exact`, `stdout-flush`, `cato-stderr`), plan 3's `cato.compile` (`compile-text` + `CRes` — created in `lib/compile.cato` by plan 3 Task 4; requiring `lib/compile.cato` from `lsp.cato` avoids any cli↔lsp cycle), `lib/json`, `lib/alist` (doc store).
- Produces:

```cato
(ns cato.lsp)
(defn serve [] :- i64 …)   ; the message loop; returns process exit code for cli.cato to exit with
```

Loop contract:
- Read headers with `stdin-getline` until empty line; EOF (empty first line) → return 0. Parse `Content-Length: N` (case-sensitive prefix per LSP; missing → `cato-stderr` note, return 1). Then `stdin-read-exact N`; EOF-short body → return 0.
- `json/parse` failure → stderr note, continue loop (never write partial output).
- Dispatch on `method` (field lookup on the message object):
  - `initialize` → respond with `{"capabilities":{"textDocumentSync":1}}` (compact; `jsonrpc`+`id` via `json/msg-of`, or `json/msg-of-null` when the request has no id — matching seed `lsp.rs:342`, which answers initialize unconditionally).
  - `initialized`, `didClose` → no response; `didClose` drops the doc from the store.
  - `shutdown` → respond `result: null`.
  - `exit` → return 0 (cli exits).
  - `didOpen`/`didChange` → upsert `AList` store keyed by `params.textDocument.uri` (`didChange` takes `contentChanges` **last** element's `text`), strip `file://` prefix → path, `(compile-text path text)`:
    - `CROk` → `publishDiagnostics` with `"diagnostics":[]`
    - `CRErr msg line col len` → one diagnostic: range `{start:{line:line-1 or 0, character:col}, end:{line:line-1 or 0, character:col+len}}` (line 0 → whole range at 0,0), `"severity":1,"source":"cato1","message":msg`, uri echoed from params. Notification: `{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{…}}`.
  - request with id + unknown method → `{"jsonrpc":"2.0","id":N,"error":{"code":-32601,"message":"method not found"}}`; unknown notification → ignore.
  - **Deliberate divergences from the seed (not covered by parity tests, which compare only definition/hover):** seed silently ignores unknown requests (`lsp.rs:426`) — kernel returns JSON-RPC `-32601` (spec §4 "minimal-but-correct"); kernel diagnostic `"source"` is `"cato1"` vs seed's `"cato"` (`lsp.rs:289`) — kernel range uses `col+len` end rather than seed's end-of-line convention (`lsp.rs:284`) — editor-facing, unconstrained by the spec.
- Write path: build body string → `print-str` exactly `"Content-Length: <count body>\r\n\r\n"` + body, then `stdout-flush` (every message, no exceptions).

- [ ] **Step 1: Write failing harness + tests (`kernel_lsp.rs`)**

```rust
// helpers:
//   frame(msg_json) -> "Content-Length: N\r\n\r\n<body>"
//   run_session(server_cmd: &mut Command, inputs: &[&str]) -> Vec<String /* bodies in order */>
//     - stdin piped; write all frames; drop stdin; read stdout to end; split frames by header+length.
//   kernel server cmd: cato-interp-run selfhost/cato/cli.cato lsp
#[test] fn initialize_responds_capabilities()  // id 1 -> result contains "textDocumentSync":1, jsonrpc 2.0
#[test] fn responds_before_stdin_closes()     // write frame(initialize); read the response from stdout with stdin STILL
                                              //   OPEN via reader-thread + recv_timeout(5s) (timeout -> kill+panic, no hang);
                                              //   then send shutdown+exit frames, drop stdin, exit 0
#[test] fn diagnostics_appear_and_clear()      // didOpen bad file (uri file:///…/t.cato) -> one diagnostic severity 1,
                                              //   exact line/character from the crafted span; didChange good text -> diagnostics:[]
#[test] fn shutdown_then_exit_zero()           // session returns child exit 0
#[test] fn malformed_body_is_ignored()         // [frame(initialize), frame("not-json"), frame(shutdown)] -> two responses,
                                              //   stdout parses as complete frames only
#[test] fn unknown_request_gets_method_not_found() // id 9 unknown -> error.code -32601
#[test] fn unknown_notification_ignored()      // "workspace/didChangeConfiguration" -> no extra output, no crash
```

Crafted diagnostic fixture: source with a reader error at a **known** span (e.g. line 3, unclosed delimiter opened at col 4 → `line 3 col 4` 1-based/0-based) → assert LSP `line:2, character:4`, end `character:4+len` — choose `len` from the reader's span (`len 1` for delimiter spans → end char 5).

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-backend-interp kernel_lsp` → FAIL

- [ ] **Step 3: Implement `lib/lsp.cato`; flip the `lsp` stub in `cli.cato`**

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-backend-interp kernel_lsp` → PASS

- [ ] **Step 5: Commit**

```bash
git add selfhost/cato crates/cato-backend-interp/tests/kernel_lsp.rs
git commit -m "feat(kernel): cato1 lsp milestone A — framing, doc sync, diagnostics"
```

---

### Task 3: Milestone B — definition + hover at seed parity

**Files:**
- Modify: `selfhost/cato/lib/lsp.cato`
- Test: extend `crates/cato-backend-interp/tests/kernel_lsp.rs`

**Interfaces:**
- Consumes: Task 2's store/loop/`json`.
- Produces: byte-identical behavior with `crates/cato-cli/src/lsp.rs` (read it; these pins are verified against its current source):
  - `offset_of(text, line, character)`: scan bytes counting `\n` until `line` is reached, then `i + character` clamped to `len` (`lsp.rs:432-443`).
  - `word_at(text, off)`: word chars = ASCII alphanumeric or `-`, `?`, `!` (`lsp.rs:448`); expand start/end over word chars; empty → not-found (`lsp.rs:446-463`).
  - `find_defn(text, name)`: for each line index `ln` (0-based), `t = line.trim_start()`; `t` starts `"(defn "` and `t`'s first whitespace-delimited token after that prefix equals `name` → return `(ln, t)` (the **trimmed** line) (`lsp.rs:467-479`).
  - `definition` result (found): `{"uri":<requested uri>,"range":{"start":{"line":L,"character":0},"end":{"line":L,"character":999}}}` (`lsp.rs:388-412`).
  - `hover` result (found): `{"contents":{"kind":"markdown","value":"```cato\n{trimmed line}\n```"}}` (`lsp.rs:413-421`).
  - Not-found (either method): result `null` (`lsp.rs:423`).
  - **Byte-equality requires compact serialization and seed key order:** top level `jsonrpc,id,result`; definition result `uri,range` with `start,end` / `line,character`; hover result `contents` with `kind,value`; capabilities `textDocumentSync,definitionProvider,hoverProvider` (all exactly as the seed's `ser()` at `lsp.rs:60-82` and body construction at `lsp.rs:341-424`).

- [ ] **Step 1: Write failing tests (extend `kernel_lsp.rs`)**

```rust
#[test] fn definition_and_hover_parity_with_seed() {
    // ONE document: "(defn foo [x :- i64] :- i64 x)\n(defn main [] (print (foo 1)))\n"
    // inputs: initialize(id1), didOpen, definition(id2, {line:0,character:6}), hover(id3, {line:0,character:6}), shutdown, exit
    // run against seed `cato lsp` AND kernel `cato-interp-run … lsp`
    // assert: same id -> same response body for id2 and id3 (byte-equal after both are framed+parsed)
}
#[test] fn definition_not_found_matches_seed()   // word with no defn: same not-found bodies from both servers
#[test] fn capabilities_now_include_definition_and_hover() // kernel initialize result contains both keys
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-backend-interp kernel_lsp` → FAIL (kernel advertises nothing yet / empty results)

- [ ] **Step 3: Implement `word-at`/`find-defn`/responses in `lsp.cato`**; add the two capability keys.

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-backend-interp kernel_lsp` → PASS (including seed-vs-kernel equality)

- [ ] **Step 5: Commit**

```bash
git add selfhost/cato/lib/lsp.cato crates/cato-backend-interp/tests/kernel_lsp.rs
git commit -m "feat(kernel): lsp milestone B — definition/hover at seed parity"
```

---

### Task 4: Seed LSP framing fix (scope addition — flagged for plan review)

**Files:**
- Modify: `crates/cato-cli/src/lsp.rs` (`run()` at ~line 312: replace `read_to_end` with incremental header/body loop, same dispatch)
- Test: `crates/cato-cli/tests/lsp.rs` (new)

**Interfaces:**
- Consumes: the existing message dispatch/capabilities/definition/hover code (unchanged).
- Produces: the seed server responds to a message **while stdin stays open** — required for real editors and for the shared harness pattern to be honest (the current server answers only after EOF; spec §4 explicitly called the kernel must not copy this).

- [ ] **Step 1: Write the failing interleaved test (`tests/lsp.rs`)**

```rust
// spawn `cato lsp` with piped stdin/stdout;
// start a reader thread that parses framed responses and sends bodies over a channel;
// write frame(initialize id 1); main thread `recv_timeout(5s)` on the channel
//   PRE-FIX: timeout -> kill child, panic "seed did not respond before stdin closed" (legible failure, no hang)
//   POST-FIX: body arrives -> assert it parses and has id 1;
// then write shutdown + exit frames, drop stdin, join/wait -> exit 0
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cato-cli --test lsp` → FAIL in ≤5s (timeout branch kills the child and panics with the message above; never a bare hang)

- [ ] **Step 3: Rewrite `run()`** — loop: read header lines until blank (EOF → return), parse Content-Length, read exactly N bytes, dispatch, write framed response, repeat. Behavior of dispatch/capabilities untouched.

- [ ] **Step 4: Run to verify passing** — `cargo test -p cato-cli --test lsp && cargo test --workspace` → PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cato-cli
git commit -m "fix(seed): LSP responds with stdin still open — incremental framing"
```

---

### Task 5: Acceptance (spec §4/§7)

- [ ] **Step 1: Both-server harness green**

Run: `cargo test -p cato-backend-interp kernel_lsp && cargo test -p cato-cli lsp`
Expected: PASS — kernel milestones A+B, seed framing + parity.

- [ ] **Step 2: Regression sweep**

Run: `cargo test --workspace && tools/kernel_gate.sh`
Expected: PASS; `GATE: PASS`, `FIXPOINT: OK` (the `lsp` flip must not disturb the build path — `lib/compile.cato` already existed from plan 3).

- [ ] **Step 3: Spec check**

- Milestone A capabilities do not advertise definition/hover before Task 3's change — verified by Task 2 test #1 evolving in Task 3.
- `rg 'read_to_end' crates/cato-cli/src/lsp.rs` → no matches.
- Kernel stdout purity: Task 2's malformed-frame test asserts every stdout frame parses.
