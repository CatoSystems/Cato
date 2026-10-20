#!/usr/bin/env bash
# mem_bench.sh — reproducible compiler memory/time baseline.
#
# Measures the compiler *process only* (--emit-ir stops before clang), for
# three compilers compiling the kernel source selfhost/cato/cli.cato:
#   seed  target/release/cato           (Rust)
#   k1    seed-built kernel             (codegen: seed's LLVM backend)
#   k2    k1-built kernel               (codegen: the kernel's own emitter)
# k1 vs k2 isolates the two code generators' memory behaviour on the same
# program; seed vs k1 is the Rust-vs-Cato implementation gap.
#
# Per compiler: wall seconds, max RSS (/usr/bin/time -l), and from
# tools/alloc_live.c: peak live heap, live at exit, churn, alloc/free calls.
# Usage: tools/mem_bench.sh [outdir]      (default: /tmp/mem_bench)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${1:-/tmp/mem_bench}"
mkdir -p "$OUT"
SRC=selfhost/cato/cli.cato
SEED=./target/release/cato
[ -x "$SEED" ] || { echo "build the release seed: cargo build -p cato-cli --release"; exit 2; }

clang -dynamiclib -O2 -o "$OUT/alloc_live.dylib" tools/alloc_live.c || exit 2

"$SEED" compile "$SRC" -o "$OUT/k1" >/dev/null 2>&1 || { echo "seed build of k1 failed"; exit 2; }
"$OUT/k1" compile "$SRC" -o "$OUT/k2" >/dev/null 2>&1 || { echo "k1 build of k2 failed"; exit 2; }

measure() { # name compiler
  local name=$1 bin=$2
  /usr/bin/time -l "$bin" compile "$SRC" --emit-ir -o "$OUT/$name.ll" 2>"$OUT/$name.time" >/dev/null
  DYLD_INSERT_LIBRARIES="$OUT/alloc_live.dylib" "$bin" compile "$SRC" --emit-ir -o "$OUT/$name.ll" \
    2>"$OUT/$name.live" >/dev/null
  local wall rss pk ex rq ac fr
  wall=$(awk '/real/{print $1; exit}' "$OUT/$name.time")
  rss=$(awk '/maximum resident set size/{print $1}' "$OUT/$name.time")
  pk=$(awk '/LIVE_PEAK/{print $2}' "$OUT/$name.live")
  ex=$(awk '/LIVE_EXIT/{print $2}' "$OUT/$name.live")
  rq=$(awk '/REQ_BYTES/{print $2}' "$OUT/$name.live")
  ac=$(awk '/ALLOCS/{print $2}' "$OUT/$name.live")
  fr=$(awk '/ALLOCS/{print $4}' "$OUT/$name.live")
  printf "%-5s wall=%6ss rss=%5dMiB peak_live=%5dMiB exit_live=%5dMiB churn=%6dMiB allocs=%10d frees=%10d\n" \
    "$name" "$wall" $((rss / 1048576)) $((pk / 1048576)) $((ex / 1048576)) $((rq / 1048576)) "$ac" "$fr"
}
measure seed "$SEED"
measure k1 "$OUT/k1"
measure k2 "$OUT/k2"
ls -l "$OUT/k1" "$OUT/k2" | awk '{print "binary", $NF, $5}'
cmp -s "$OUT/k1.ll" "$OUT/k2.ll" && echo "k1/k2 IR: identical" || echo "k1/k2 IR: DIFFER"
