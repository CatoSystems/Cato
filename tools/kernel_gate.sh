#!/usr/bin/env bash
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SEED=./target/debug/cato
FX=selfhost/fixtures/kernel
fail=0

[ -x "$SEED" ] || { echo "seed missing: cargo build -p cato"; exit 2; }

# run_* too: kb() verifies acceptance by artifact existence, so a stale
# fixture binary from an earlier run fakes an OK (this cost a debugging
# cycle — the gate reported PASS while k1 rejected bk1/bk3).
rm -f /tmp/k1 /tmp/k2 /tmp/k2.ll /tmp/k3 /tmp/k3.ll /tmp/k2build.log /tmp/run_*

# Seed (Rust) accepts `-o`; the kernel takes a positional output and exits 0
# even when cc fails, so every kernel build is verified by its artifact.
"$SEED" compile selfhost/cato/cli.cato -o /tmp/k1 || { echo "SEED BUILD FAIL"; exit 2; }

kb() { # src out   (kernel build; true iff artifact exists)
  /tmp/k1 build "$1" "$2" >/tmp/kb.log 2>&1
  [ -x "$2" ]
}

# Deterministic allocation-churn metric. Peak RSS via `/usr/bin/time -l` is
# unusable here: it folds in the clang child and swings ~50% run to run. We
# instead interpose malloc/realloc in the kernel process only. The seed-built
# k1 is not hardened, so DYLD_INSERT_LIBRARIES loads; clang is hardened, so its
# child allocations are stripped. ALLOC_BYTES is byte-stable across runs.
clang -dynamiclib -O2 -o /tmp/alloc_count.dylib tools/alloc_count.c \
  || { echo "ALLOC COUNTER BUILD FAIL"; exit 2; }

# The k2 self-build is where churn is measured.
DYLD_INSERT_LIBRARIES=/tmp/alloc_count.dylib /tmp/k1 build selfhost/cato/cli.cato /tmp/k2 >/tmp/k2build.log 2>&1
[ -x /tmp/k2 ] || { echo "KERNEL BUILD k2 FAIL"; tail -2 /tmp/k2build.log; exit 2; }
read -r ALLOC CALLS <<<"$(awk '/ALLOC_BYTES/{b=$2;c=$4} END{print b, c}' /tmp/k2build.log)"

# k3 is built by the kernel-generated k2 — the self-hosting fixpoint.
/tmp/k2 build selfhost/cato/cli.cato /tmp/k3 >/tmp/k3build.log 2>&1
[ -x /tmp/k3 ] || { echo "KERNEL BUILD k3 FAIL"; tail -2 /tmp/k3build.log; exit 2; }

if [ -f /tmp/k2.ll ] && [ -f /tmp/k3.ll ] && cmp -s /tmp/k2.ll /tmp/k3.ll; then
  echo "FIXPOINT: OK"
else
  echo "FIXPOINT: DIFF"; fail=1
fi

check_out() { # file expected
  kb "$FX/$1.cato" "/tmp/run_$1" || { echo "BUILD FAIL $1"; fail=1; return; }
  got="$(/tmp/run_$1)"; [ "$got" = "$2" ] && echo "OK $1" || { echo "OUTPUT FAIL $1: '$got' != '$2'"; fail=1; }
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
  kb "$FX/$1.cato" "/tmp/run_$1" || { echo "BUILD FAIL $1"; fail=1; return; }
  "/tmp/run_$1" >/tmp/run_$1.out 2>/dev/null &
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
  got="$(cat /tmp/run_$1.out)"
  [ "$got" = "$2" ] && echo "OK $1" || { echo "OUTPUT FAIL $1: '$got' != '$2'"; fail=1; }
}
check_out_bounded vec_loop "$(printf '300000\n299999\n0')" 20

check_ok()  { kb "$FX/$1.cato" "/tmp/run_$1" && echo "OK $1" || { echo "ACCEPT FAIL $1"; fail=1; }; }
check_bad() { kb "$FX/$1.cato" "/tmp/run_$1" && { echo "REJECT FAIL $1"; fail=1; } || echo "OK $1(rejected)"; }
check_ok bk1; check_bad bk2; check_ok bk3; check_bad bk4
# Loader contract (parity with the seed): duplicate ns, an unbound alias,
# and a malformed require spec are all build errors in the kernel too.
check_bad dupns; check_bad badalias; check_bad malreq

echo "ALLOC_BYTES: $ALLOC"
echo "ALLOC_CALLS: $CALLS"
echo "GATE: $([ $fail -eq 0 ] && echo PASS || echo FAIL)"
exit $fail
