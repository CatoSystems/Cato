# Cato Standard Library

The standard library lives under `selfhost/cato/std/` and is ordinary Cato compiled by the seed compiler. Each module is namespaced `cato.std.<name>`.

## Modules

- `core` — constants, identity, assertions, Option/Result helpers (i64).
- `num` — checked/saturating/wrapping arithmetic, bit ops, popcount, remainder.
- `math` — sqrt, pow, floor, ceil, round, gcd, lcm, factorial.
- `str` — string predicates, case conversion, trim, search, replace, pad, parse-bool.
- `vec` — vector helpers: first/last, reverse, concat, take/drop/slice, sort, dedup, sum/min/max, range.
- `map` — map helpers (count, contains?, assoc).
- `set` — set helpers.
- `list` — cons lists.
- `bitset` — 64-bit bitset.
- `path` — lexical path manipulation.
- `env` — environment variables.
- `fs` — file read/write/exists/delete/copy.
- `io` — console I/O.
- `process` — spawn.
- `error` — error helpers over Result.
- `test` — check/assert-eq.
- `log` — logging to stderr.

## Usage

```clojure
(require ["std/core.cato" :as core])  ; adjust path
```

When compiling from repo root, use relative path `selfhost/cato/std/core.cato`.

## Planned (not yet implemented)

Concurrency (threads, channels), networking (TCP/UDP/HTTP), JSON, URL parsing, arenas/regions, Unicode normalization, directory listing, time/clocks, sleep, TLS/crypto (via external libs). These require new runtime primitives or large pure-Cato implementations; see plan.