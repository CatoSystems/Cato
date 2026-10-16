#!/usr/bin/env bash
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SEED=./target/debug/cato
FX=selfhost/fixtures/kernel
fail=0

[ -x "$SEED" ] || { echo "seed missing: cargo build -p cato"; exit 2; }

python3 tools/concat_kernel.py || exit 2

rm -f /tmp/k1 /tmp/k2 /tmp/k2.ll /tmp/k3 /tmp/k3.ll /tmp/k2build.log

# Seed (Rust) accepts `-o`; the kernel takes a positional output and exits 0
# even when cc fails, so every kernel build is verified by its artifact.
"$SEED" build selfhost/cato.cato -o /tmp/k1 || { echo "SEED BUILD FAIL"; exit 2; }

kb() { # src out   (kernel build; true iff artifact exists)
  /tmp/k1 build "$1" "$2" >/tmp/kb.log 2>&1
  [ -x "$2" ]
}

# The k2 self-build is where peak RSS is measured.
/usr/bin/time -l /tmp/k1 build selfhost/cato.cato /tmp/k2 >/tmp/k2build.log 2>&1
[ -x /tmp/k2 ] || { echo "KERNEL BUILD k2 FAIL"; tail -2 /tmp/k2build.log; exit 2; }
RSS=$(awk '/maximum resident set size/{print $1}' /tmp/k2build.log)

# k3 is built by the kernel-generated k2 — the self-hosting fixpoint.
/tmp/k2 build selfhost/cato.cato /tmp/k3 >/tmp/k3build.log 2>&1
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

check_ok()  { kb "$FX/$1.cato" "/tmp/run_$1" && echo "OK $1" || { echo "ACCEPT FAIL $1"; fail=1; }; }
check_bad() { kb "$FX/$1.cato" "/tmp/run_$1" && { echo "REJECT FAIL $1"; fail=1; } || echo "OK $1(rejected)"; }
check_ok bk1; check_bad bk2; check_ok bk3; check_bad bk4

echo "MAXRSS_BYTES: $RSS"
echo "GATE: $([ $fail -eq 0 ] && echo PASS || echo FAIL)"
exit $fail
