#!/usr/bin/env bash
# asan_run.sh — compile a Cato program's IR at -O0 under AddressSanitizer
# and run it. (`cato compile --asan` builds at -O2, which optimizes small
# double frees away; -O0 keeps every free.) Prints the program's output
# and either "ASAN: clean" or the sanitizer report's first lines.
# Usage: tools/asan_run.sh <file.cato> [compiler]
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC=$1; CC=${2:-$ROOT/target/release/cato}
T=$(mktemp -d)
"$CC" compile "$SRC" --emit-ir --asan -o "$T/p.ll" >/dev/null || { echo "COMPILE FAIL"; rm -rf "$T"; exit 1; }
clang -O0 -g -w -fsanitize=address "$T/p.ll" "$ROOT/runtime/runtime.c" -o "$T/p" || { echo "LINK FAIL"; rm -rf "$T"; exit 1; }
"$T/p" >"$T/out" 2>"$T/err"; rc=$?
cat "$T/out"
if grep -q 'ERROR: AddressSanitizer' "$T/err"; then
  echo "ASAN: $(grep -m1 'ERROR: AddressSanitizer' "$T/err" | sed 's/.*AddressSanitizer: //')"
  grep -m3 ' in ' "$T/err" | sed 's/^/    /'
  rm -rf "$T"; exit 3
fi
echo "ASAN: clean (exit $rc)"
rm -rf "$T"
