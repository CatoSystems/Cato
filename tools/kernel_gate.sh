#!/usr/bin/env bash
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SEED=./target/debug/cato
# Scratch directory for kernel binaries and fixture artifacts. Defaults to
# /tmp; set CATO_GATE_TMP to a private directory when another checkout may
# run the gate concurrently (the fixed paths would clobber each other).
T="${CATO_GATE_TMP:-/tmp}"
mkdir -p "$T"
FX=selfhost/fixtures/kernel
fail=0

[ -x "$SEED" ] || { echo "seed missing: cargo build -p cato"; exit 2; }

# run_* too: kb() verifies acceptance by artifact existence, so a stale
# fixture binary from an earlier run fakes an OK (this cost a debugging
# cycle — the gate reported PASS while k1 rejected bk1/bk3).
rm -f $T/k1 $T/k2 $T/k2.ll $T/k3 $T/k3.ll $T/k2build.log $T/run_*

# Every kernel build is also verified by its artifact (exit codes alone
# once hid a stale binary, see above).
"$SEED" compile selfhost/cato/cli.cato -o $T/k1 || { echo "SEED BUILD FAIL"; exit 2; }

kb() { # src out   (kernel build; true iff artifact exists)
  $T/k1 compile "$1" -o "$2" >$T/kb.log 2>&1
  [ -x "$2" ]
}

# Deterministic allocation-churn metric. Peak RSS via `/usr/bin/time -l` is
# unusable here: it folds in the clang child and swings ~50% run to run. We
# instead interpose malloc/realloc in the kernel process only. The seed-built
# k1 is not hardened, so DYLD_INSERT_LIBRARIES loads; clang is hardened, so its
# child allocations are stripped. ALLOC_BYTES is byte-stable across runs.
clang -dynamiclib -O2 -o $T/alloc_count.dylib tools/alloc_count.c \
  || { echo "ALLOC COUNTER BUILD FAIL"; exit 2; }

# The k2 self-build is where churn is measured.
DYLD_INSERT_LIBRARIES=$T/alloc_count.dylib $T/k1 compile selfhost/cato/cli.cato -o $T/k2 --emit-ll $T/k2.ll >$T/k2build.log 2>&1
[ -x $T/k2 ] || { echo "KERNEL BUILD k2 FAIL"; tail -2 $T/k2build.log; exit 2; }
read -r ALLOC CALLS <<<"$(awk '/ALLOC_BYTES/{b=$2;c=$4} END{print b, c}' $T/k2build.log)"

# k3 is built by the kernel-generated k2 — the self-hosting fixpoint.
$T/k2 compile selfhost/cato/cli.cato -o $T/k3 --emit-ll $T/k3.ll >$T/k3build.log 2>&1
[ -x $T/k3 ] || { echo "KERNEL BUILD k3 FAIL"; tail -2 $T/k3build.log; exit 2; }

if [ -f $T/k2.ll ] && [ -f $T/k3.ll ] && cmp -s $T/k2.ll $T/k3.ll; then
  echo "FIXPOINT: OK"
else
  echo "FIXPOINT: DIFF"; fail=1
fi

# Memory safety of the kernel's own planner: k2's IR (planned and emitted
# by the kernel) rebuilt with AddressSanitizer at -O0, compiling the kernel.
# The allocator alone does not reliably abort on a double free, so a
# planner bug can otherwise pass the fixpoint. The -O0 frames are large:
# the main thread gets a 512 MiB stack. CATO_GATE_NO_ASAN=1 skips it.
if [ -z "${CATO_GATE_NO_ASAN:-}" ]; then
  sed -E 's/^(define .*\)) \{$/\1 #97 {/' $T/k2.ll > $T/k2asan.ll
  echo 'attributes #97 = { sanitize_address }' >> $T/k2asan.ll
  if clang -O0 -w -fsanitize=address -Wl,-stack_size,0x20000000 $T/k2asan.ll runtime/runtime.c -o $T/k2asan \
     && $T/k2asan compile selfhost/cato/cli.cato --emit-ir -o $T/k3asan.ll >/dev/null 2>$T/k2asan.err \
     && cmp -s $T/k3asan.ll $T/k3.ll; then
    echo "ASAN k2: OK"
  else
    echo "ASAN k2: FAIL ($(grep -m1 'ERROR: AddressSanitizer' $T/k2asan.err | sed 's/.*AddressSanitizer: //'))"; fail=1
  fi
fi

check_out() { # file expected
  kb "$FX/$1.cato" "$T/run_$1" || { echo "BUILD FAIL $1"; fail=1; return; }
  got="$($T/run_$1)"; [ "$got" = "$2" ] && echo "OK $1" || { echo "OUTPUT FAIL $1: '$got' != '$2'"; fail=1; }
}
check_out t1 "la=X"; check_out t2 "X"; check_out t3 "7"
check_out t4 "3";    check_out t5 "3"; check_out t6 "3"
check_out mod "$(printf '49\n25\n16')"
check_out xd "$(printf '9\n16')"
check_out cycle "5"

# vec_loop also guards the loop-conj capacity fix: correctness alone would
# pass a quadratic kernel, so bound the run. A kernel that re-grows from
# len each recur needs ~2n^2*8 bytes here and is killed long before the
# bound; the fixed one finishes in milliseconds.
check_out_bounded() { # file expected maxsec
  kb "$FX/$1.cato" "$T/run_$1" || { echo "BUILD FAIL $1"; fail=1; return; }
  "$T/run_$1" >$T/run_$1.out 2>/dev/null &
  pid=$!
  waited=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 0.2; waited=$((waited + 1))
    if [ "$waited" -gt $(($3 * 5)) ]; then
      kill -9 "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
      echo "TIMEOUT FAIL $1 (>${3}s)"; fail=1; return
    fi
  done
  wait "$pid"
  got="$(cat $T/run_$1.out)"
  [ "$got" = "$2" ] && echo "OK $1" || { echo "OUTPUT FAIL $1: '$got' != '$2'"; fail=1; }
}
check_out_bounded vec_loop "$(printf '300000\n299999\n0')" 20

check_ok()  { kb "$FX/$1.cato" "$T/run_$1" && echo "OK $1" || { echo "ACCEPT FAIL $1"; fail=1; }; }
check_bad() { kb "$FX/$1.cato" "$T/run_$1" && { echo "REJECT FAIL $1"; fail=1; } || echo "OK $1(rejected)"; }
check_ok bk1; check_bad bk2; check_ok bk3; check_bad bk4
# Loader contract (parity with the seed): duplicate ns, an unbound alias,
# and a malformed require spec are all build errors in the kernel too.
check_bad dupns; check_bad badalias; check_bad malreq

# Diagnostics name the failing sub-form (file:line:col), not the defn.
check_loc() { # file expected-location
  $T/k1 compile "$FX/$1.cato" -o "$T/run_$1" >$T/kb.log 2>&1
  grep -q "$FX/$1.cato:$2: " $T/kb.log && echo "OK $1" \
    || { echo "LOCATION FAIL $1: $(head -1 $T/kb.log)"; fail=1; }
}
check_loc errloc 5:10
check_loc errloc_move 7:25
check_out capnest "5"

echo "ALLOC_BYTES: $ALLOC"
echo "ALLOC_CALLS: $CALLS"
echo "GATE: $([ $fail -eq 0 ] && echo PASS || echo FAIL)"
exit $fail
