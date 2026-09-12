# Cato Maps design (Phase 8b, bounded slice)

Maps are **i64-keyed snapshots of i64 values**: flat arrays of 16-byte
(key, value) pairs on the Phase 7b heap.

- `{1 10 2 20}` literals sort at compile time; `{}` is the empty map.
- `(get m k [default])` — single-block linear scan, branchless body; the
  **last match wins**, which gives assoc its replace semantics.
- `(assoc m k v)` — one allocation: a filter-copy pass keeps entries whose
  key differs (write-pointer compaction), then (k, v) is appended. True
  replacement: `(count (assoc m k v))` is honest even when k exists.
- `(count m)` — entry count.
- Maps are **immutable snapshots**: assoc never mutates the source, so
  aliases (sharing the buffer) stay valid — the ownership checker treats
  them as Copy. Maps are local values: they do not cross function
  boundaries yet (no annotation syntax; `map` params/returns are rejected).

Sets are deferred: `#{...}` is representable as keys of a map with dummy
values, but a bit-packed representation deserves its own slice.

## Reference program

`examples/maps.cato` → `20 3 99 99 20 3 70 4 0 -1`.
