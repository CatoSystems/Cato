# Cato HAMT Maps Design (Phase 15)

## Goal

Upgrade the flat map slice to Clojure-grade persistent structures: the
flat ≤8-entry map stays the small-case representation; larger maps live
in a 16-way persistent hash-array-mapped trie with structural sharing.

## Representation

The map header `{ptr, count, cap}` gains a representation tag in `cap`:
0 = flat sorted array (unchanged), 1 = `ptr` is a HAMT root. The tag is
invisible to semantics — the interpreter keeps flat maps forever and all
programs produce identical results on both backends (parity suite).

## Trie

C runtime (`runtime.c`): 16-way trie consuming 4 bits per level of a
splitmix-mixed key (16 levels cover 64 bits), collision chains in tagged
leaves, internal nodes with popcount-sized child arrays. `assoc` copies
only the nodes along the key's path — immutable snapshots with
structural sharing. `get` walks the same path.

## Emission

- literals: always flat (tag 0);
- `assoc`: flat and count < 8 → in-place budget copy (unchanged); count ≥ 8
  → convert via `cato_hamt_from_flat`, then `cato_hamt_assoc` (tag 1);
  already-HAMT → `cato_hamt_assoc` directly;
- `get`: branch on the tag — flat branchless scan vs runtime lookup;
- scope-exit frees (Phase 13) free only flat buffers: shared trie nodes
  live until process exit (per-node frees are unsafe under sharing, and
  unreachable tries drop whole only from the flat path).

## Deferred pieces

- `transient`/`persistent!` batch mutation and `into`: the trie supports
  them (a transient is a root flagged mutable, assoc mutating the path
  nodes in place), but the ownership interaction needs per-node unique-
  access proofs; deferred to a follow-up slice.
- Bit-packed sets on the same trie (sparse keys): deferred; sets already
  share the flat representation.

## Verification

`examples/hamt.cato` builds a 41-entry map through the conversion
boundary and asserts gets/count — identical output through clang and the
interpreter; the whole suite stays green and ASAN-clean.
