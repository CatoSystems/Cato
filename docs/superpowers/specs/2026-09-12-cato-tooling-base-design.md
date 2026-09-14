# Cato Tooling Base Design (Phase 19)

The language/tooling floor, in three slices (patterns shipped separately
as 19a/19b commits):

## REPL (`cato repl`)

A form-at-a-time frontend over the interpreter. Definitional forms
(`defn`, `deftype`, `extern`, `ns`, `require`) accumulate into the session
source and compile eagerly (failed definitions roll back); any other form
is evaluated by compiling session + `(defn __repl [] form)` and running
`__repl` in the interpreter, printing `=> value` with `%g`-style float
formatting, `[a b]` vectors, `#{...}` sets, `{k v}` maps. Recompiling per
entry is O(session) but semantically exact — the REPL runs the same
pipeline as `cato run`, so everything in `examples/` behaves identically.

## LSP (`cato lsp`)

JSON-RPC over stdio with a hand-rolled parser/serializer (the workspace
has zero dependencies by policy). Capabilities:

- full-text sync; every open/change recompiles through the real frontend
  AND the LLVM backend under `catch_unwind` — frontend errors carry their
  `[CODE] file:line:col` location (parsed into a range), backend panics
  (unknown functions, unsupported constructs) become diagnostics too;
- go-to-definition and hover over `defn` names (word-at-point, first
  matching `(defn name ...)`).

## Multi-file modules (19b)

`(ns name)` per file (names unique) and `(require [path.cato :as alias])`
resolved relative to the requiring file, merged transitively at
expansion time: required defns rename to `alias__name`, `alias/name`
call heads rewrite to match, and the required file's own internal calls
follow. Externs stay global (they name C symbols). Cycle-free by
construction (seen-file set).
