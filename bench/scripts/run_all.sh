#!/usr/bin/env bash
# Fair Cato benchmark entry point.
#
#   ./bench/scripts/run_all.sh            # full run
#   ./bench/scripts/run_all.sh --quick    # 1 build / 3 runs per binary
#   ./bench/scripts/run_all.sh --only fib_rec,vec_append
#
# Builds the release seed and the self-hosted kernel (cato1), then hands off to
# bench.py, which writes results/run_<timestamp>.md and .json.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

SEED="$ROOT/target/release/cato"
KERNEL="$ROOT/target/bench/compilers/cato1"

echo "== building release seed (cargo build --release -p cato-cli) =="
cargo build --release -p cato-cli >/dev/null

echo "== building kernel cato1 =="
# The kernel entry moved from the concatenation to the require'd cli module.
KERNEL_SRC="selfhost/cato/cli.cato"
[ -f "$KERNEL_SRC" ] || KERNEL_SRC="selfhost/cato.cato"
mkdir -p "$(dirname "$KERNEL")"
"$SEED" build "$KERNEL_SRC" -o "$KERNEL" >/dev/null

STAMP="$(date +%Y%m%d_%H%M%S)"
MD="$ROOT/bench/results/run_${STAMP}.md"
JSON="$ROOT/bench/results/run_${STAMP}.json"
mkdir -p "$ROOT/bench/results"

python3 "$ROOT/bench/scripts/bench.py" "--md=$MD" "--json=$JSON" "$@"
echo "wrote $MD"
echo "wrote $JSON"
