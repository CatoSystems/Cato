# HANDOFF: Kernel Clone Elimination & Memory Churn Optimization

## Status
- Branch: main (commits 2aaa847,3b00c7f,038863f,85be8f7,2103cc3,f8eb13b,ee76171,9a99f8e,cda85b2,ba40c66,bf12950)
- Kernel self-build alloc churn: 144 GB → 50.6 GB (65% reduction). ALLOC_CALLS 3.19B → 1.19B.
- Gate: PASS (deterministic ALLOC_BYTES/ALLOC_CALLS, fixpoint k1==k2==k3, all fixtures pass t1–t6, bk1/bk3 ok, bk2/bk4 rejected).
- Seed release peak RSS building kernel: ~209–211 MB; kernel self-build churn measured 50.6 GB.
- Tests: 13/13 unclone.py tests pass. interp kernel_emit has a pre-existing unrelated failure (do not block).

## What works
- Deterministic metric: tools/alloc_count.c + DYLD_INSERT_LIBRARIES; gate parses ALLOC_BYTES/ALLOC_CALLS (no MAXRSS noise).
- unclone.py semantics-preserving (only removes `(clone X)` when safe): pure-unwrap verifier, guards for borrowed params, match-scrutinee protection, owner-move vs field-read, bare-use awareness, idempotent.
- Removed clones across moveck, lower, forms, emit/*, cli, hir/*, reader/*, lib/* (4272 total down substantially).
- Fast iterate: transform→purity-verify→concat→seed build→fix→gate.

## Remaining work (deeper)
- Kill more clones (2111 still in selfhost/cato.cato; top vars tab/ps/env/adts…). Look for patterns: `(clone ps2)` after match destructure, cloned temps passed to calls that don’t move, etc.
- Find ownership/semantics wins: avoid creating unnecessary aliases that force clones. The unclone guards are conservative; can extend.
- Investigate memory leaks: add leak detection/forbidden patterns in Cato (borrow checker doesn’t currently flag leaks). Consider integrating checks (maybe track drops/consumption more strictly) without breaking self-hosting.
- Algorithmic hotspots: lower, moveck, forms, emit do lots of PS/threading and cloning. Consider redesigning PS to reduce copying.
- Goal: approach seed’s behavior/resource profile.

## Files to focus
- tools/unclone.py (evolutionary guards)
- selfhost/cato/hir/lower.cato, forms.cato, moveck.cato, emit/*.cato (top hotspots)
- crates/cato-core/src/hir/ownership.rs (checker) if adding leak rules
- tools/alloc_count.c, tools/kernel_gate.sh

## Quick commands
```bash
python3 tools/concat_kernel.py && DYLD_INSERT_LIBRARIES=/tmp/alloc_count.dylib /tmp/k1_rel build selfhost/cato.cato /tmp/k2 2>&1 | grep ALLOC
./tools/kernel_gate.sh | tail -10
python3 -m pytest tools/tests/test_unclone.py -q
cargo build --release
```

## Next agent todo
1. Profile top clone sites in cato.cato, identify safe elisions beyond unclone.py (or extend it). 2. Try to reduce churn further while keeping gate PASS. 3. Explore adding Cato-level leak guards (ownership semantics) to catch leaks at compile time. 4. Keep changes semantics-preserving; run gate after each batch.
