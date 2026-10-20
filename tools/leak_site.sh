#!/usr/bin/env bash
# leak_site.sh — build a Cato program with frame records and attribute what
# it leaves live at exit (tools/alloc_site.c). For probing planner leaks.
# Usage: tools/leak_site.sh <file.cato> [compiler]   (default: release seed)
#   compiler may be a kernel binary (k1/k2) to probe the kernel's planner.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC=$1; CC=${2:-$ROOT/target/release/cato}
T=$(mktemp -d)
clang -dynamiclib -O2 -o "$T/site.dylib" "$ROOT/tools/alloc_site.c"
"$CC" compile "$SRC" --emit-ir -o "$T/p.ll" >/dev/null
sed -E 's/^(define .*\)) \{$/\1 #99 {/' "$T/p.ll" > "$T/pf.ll"
echo 'attributes #99 = { "frame-pointer"="all" }' >> "$T/pf.ll"
clang -O0 -w "$T/pf.ll" "$ROOT/runtime/runtime.c" -o "$T/p"
DYLD_INSERT_LIBRARIES="$T/site.dylib" ALLOC_SITE_OUT="$T/site" "$T/p" >/dev/null
head -${LEAK_TOP:-25} "$T/site.exit"
rm -rf "$T"
