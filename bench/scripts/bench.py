#!/usr/bin/env python3
"""Fair benchmark harness: Cato (seed + kernel) vs Rust / Go / Zig.

Measures, for every benchmark program:

  * compile time  — wall time to build the native binary (min of --builds);
  * run time      — wall time of the produced binary, median of --runs runs;
  * peak RSS      — max resident set of one run, in MiB.

Fairness rules (long form in bench/README.md):

  * Identical algorithm and workload in every language.
  * The seed <-> kernel head-to-head uses the same clang level (-O2), because
    the kernel CLI pins `clang -O2` in selfhost/cato/cli.cato; the seed is also
    reported at -O3 (its best) for the language-vs-language table. Rust is
    -C opt-level=3, Go and Zig use their standard release settings.
  * One warm-up run is discarded; run time is the median of --runs runs.
  * Compile time includes the C / link step for every language.

Run directly, or via scripts/run_all.sh:

    python3 bench/scripts/bench.py --quick
    python3 bench/scripts/bench.py --only fib_rec,mandelbrot --runs 10
    python3 bench/scripts/bench.py --json out.json --md out.md
"""

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent.parent
ROOT = BENCH_DIR.parent

SEED = ROOT / "target/release/cato"
KERNEL = ROOT / "target/bench/compilers/cato1"
RUNTIME = ROOT / "runtime/runtime.c"
BIN_DIR = ROOT / "target/bench/bin"
GO_DIR = BENCH_DIR / "equivalents/go"
RESULTS_DIR = BENCH_DIR / "results"

# (name, tier, expected stdout).  shared => seed AND kernel compile the same
# Cato source; full => seed-only (reduce/fused macros the kernel lacks).
BENCHMARKS = [
    ("fib_rec", "shared", "3524578"),
    ("fib_iter", "shared", "945351196"),
    ("str_bytes", "shared", "150500000000"),
    ("vec_append", "shared", "200000"),
    ("tree", "shared", "500000500000"),
    ("mandelbrot", "shared", "21108556"),
    ("fused_sum", "full", "19999999900000000"),
    ("pipe", "full", "4499995500001000000"),
]

# Modes shown, in display order.  Cato seed -O2 and Cato kernel -O2 share the
# clang level, isolating the two compiler implementations.
MODES = ["cato-seed-o3", "cato-seed-o2", "cato-kernel", "rust", "go", "zig"]
MODE_LABEL = {
    "cato-seed-o3": "Cato seed -O3",
    "cato-seed-o2": "Cato seed -O2",
    "cato-kernel": "Cato kernel -O2",
    "rust": "Rust -O3",
    "go": "Go release",
    "zig": "Zig ReleaseFast",
}
TOOL = {
    "cato-seed-o3": None,
    "cato-seed-o2": None,
    "cato-kernel": None,
    "rust": "rustc",
    "go": "go",
    "zig": "zig",
}


def ru_to_mib(ru_maxrss: int) -> float:
    # macOS reports bytes; Linux reports KiB.
    if platform.system() == "Darwin":
        return ru_maxrss / (1024 * 1024)
    return ru_maxrss / 1024.0


def targets_for(name, tier):
    """Return {mode: {'cmd': [...], 'cwd': str|None, 'out': Path}}."""
    src = BENCH_DIR / "sources" / tier / f"{name}.cato"
    t = {}

    def add(mode, cmd, out, cwd=None):
        t[mode] = {"cmd": [str(c) for c in cmd], "cwd": str(cwd) if cwd else None, "out": Path(out)}

    add("cato-seed-o2", [SEED, "compile", src, "-o", BIN_DIR / f"{name}.seed-o2"], BIN_DIR / f"{name}.seed-o2")
    add("cato-seed-o3", [SEED, "compile", src, "-O3", "-o", BIN_DIR / f"{name}.seed-o3"], BIN_DIR / f"{name}.seed-o3")
    if tier == "shared" and KERNEL.exists():
        add("cato-kernel", [KERNEL, "compile", src, "-O2", "-o", BIN_DIR / f"{name}.kernel", "--runtime", RUNTIME],
            BIN_DIR / f"{name}.kernel", cwd=ROOT)
    rs = BENCH_DIR / "equivalents" / "rust" / f"{name}.rs"
    if rs.exists():
        add("rust", ["rustc", "-C", "opt-level=3", rs, "-o", BIN_DIR / f"{name}.rust"], BIN_DIR / f"{name}.rust")
    if (GO_DIR / name / "main.go").exists():
        add("go", ["go", "build", "-o", BIN_DIR / f"{name}.go", f"./{name}"], BIN_DIR / f"{name}.go", cwd=GO_DIR)
    zz = BENCH_DIR / "equivalents" / "zig" / f"{name}.zig"
    if zz.exists():
        add("zig", ["zig", "build-exe", "-O", "ReleaseFast", "-lc", zz,
                    f"-femit-bin={BIN_DIR / f'{name}.zig'}"], BIN_DIR / f"{name}.zig")
    return t


def compile_best(t, builds):
    """Build `builds` times, return min wall seconds or None on failure."""
    best = None
    for _ in range(max(1, builds)):
        try:
            t["out"].unlink()
        except FileNotFoundError:
            pass
        start = time.perf_counter()
        r = subprocess.run(t["cmd"], cwd=t["cwd"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wall = time.perf_counter() - start
        # The kernel CLI exits 0 even when cc fails: verify the artifact.
        if r.returncode != 0 or not t["out"].exists() or t["out"].stat().st_size == 0:
            return None
        best = wall if best is None else min(best, wall)
    return best


def capture(binary):
    try:
        # Zig's std.debug.print writes to stderr, so capture both streams.
        r = subprocess.run([str(binary)], stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           cwd=str(ROOT), timeout=600)
        return r.stdout.decode(errors="replace").strip()
    except Exception as e:  # noqa: BLE001
        return f"<error: {e}>"


def measure_runs(binary, runs):
    """Warm-up + timed runs; returns dict or None on failure."""
    warm = subprocess.run([str(binary)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(ROOT))
    if warm.returncode != 0:
        return None
    walls, peaks = [], []
    for _ in range(runs):
        start = time.perf_counter()
        p = subprocess.Popen([str(binary)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(ROOT))
        _, status, ru = os.wait4(p.pid, 0)
        walls.append(time.perf_counter() - start)
        peaks.append(ru_to_mib(ru.ru_maxrss))
        if os.waitstatus_to_exitcode(status) != 0:
            return None
    return {
        "median_ms": statistics.median(walls) * 1000.0,
        "min_ms": min(walls) * 1000.0,
        "rss_mib": max(peaks),
        "runs": runs,
    }


def _first_line(args):
    try:
        return subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout.decode().splitlines()[0].strip()
    except Exception:  # noqa: BLE001
        return "?"


def _sha12(p):
    try:
        return hashlib.sha256(Path(p).read_bytes()).hexdigest()[:12]
    except Exception:  # noqa: BLE001
        return "?"


def env_info():
    cpu = _first_line(["sysctl", "-n", "machdep.cpu.brand_string"]) if platform.system() == "Darwin" else platform.processor()
    return {
        "os": platform.platform(),
        "arch": platform.machine(),
        "cpu": cpu,
        "clang": _first_line(["clang", "--version"]),
        "rustc": _first_line(["rustc", "--version"]),
        "go": _first_line(["go", "version"]),
        "zig": _first_line(["zig", "version"]),
        "seed_sha256_12": _sha12(SEED),
        "kernel_sha256_12": _sha12(KERNEL),
        "git_rev": _first_line(["git", "-C", str(ROOT), "rev-parse", "--short", "HEAD"]),
    }


def fmt_ms(v):
    if v is None:
        return "—"
    if v >= 1000:
        return f"{v/1000:.2f} s"
    if v >= 10:
        return f"{v:.1f} ms"
    return f"{v:.2f} ms"


def fmt_mib(v):
    return "—" if v is None else f"{v:.1f}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=7, help="timed runs per binary (warm-up excluded)")
    ap.add_argument("--builds", type=int, default=3, help="builds per compile-time sample (min taken)")
    ap.add_argument("--quick", action="store_true", help="1 build, 3 runs")
    ap.add_argument("--only", default="", help="comma-separated benchmark names")
    ap.add_argument("--json", default="", help="write JSON results here")
    ap.add_argument("--md", default="", help="write Markdown report here")
    args = ap.parse_args()
    if args.quick:
        args.builds, args.runs = 1, 3

    if not SEED.exists():
        sys.exit(f"seed compiler missing: {SEED}\n  build it with: cargo build --release -p cato")
    if not KERNEL.exists():
        print(f"warning: kernel compiler missing ({KERNEL}); shared tier will skip it", file=sys.stderr)

    BIN_DIR.mkdir(parents=True, exist_ok=True)
    only = {s for s in args.only.split(",") if s}
    missing_tools = {m for m, tool in TOOL.items() if tool and not shutil.which(tool)}
    if missing_tools:
        print(f"note: skipping modes with missing tools: {sorted(missing_tools)}", file=sys.stderr)

    results = {"env": env_info(), "config": {"runs": args.runs, "builds": args.builds}, "benchmarks": {}}

    for name, tier, expected in BENCHMARKS:
        if only and name not in only:
            continue
        row = {"tier": tier, "expected": expected, "modes": {}}
        tgts = targets_for(name, tier)
        print(f"\n== {name} [{tier}] ==", file=sys.stderr)
        for mode in MODES:
            if mode not in tgts or mode in missing_tools:
                continue
            t = tgts[mode]
            comp = compile_best(t, args.builds)
            rec = {"compile_ms": comp * 1000.0 if comp is not None else None}
            if comp is None:
                print(f"   {mode:14s} BUILD FAILED", file=sys.stderr)
            else:
                out = capture(t["out"])
                rec["stdout"] = out
                rec["match"] = (out == expected)
                run = measure_runs(t["out"], args.runs)
                if run is None:
                    print(f"   {mode:14s} build ✔  run FAILED  out={out!r}", file=sys.stderr)
                else:
                    rec.update(run)
                    note = "match" if rec["match"] else f"MISMATCH {out!r}"
                    print(f"   {mode:14s} build {fmt_ms(rec['compile_ms']):>9}  "
                          f"run {fmt_ms(run['median_ms']):>9}  rss {fmt_mib(run['rss_mib']):>7} MiB  {note}",
                          file=sys.stderr)
            row["modes"][mode] = rec
        results["benchmarks"][name] = row

    report = render_md(results)
    print(report)
    if args.md:
        Path(args.md).parent.mkdir(parents=True, exist_ok=True)
        Path(args.md).write_text(report)
    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2))
    return 0


def render_md(results):
    env = results["env"]
    active = [m for m in MODES if m in any_modes(results)]
    lines = ["# Cato benchmark results", ""]
    lines.append(f"- machine: {env['cpu']} ({env['arch']}), {env['os']}")
    lines.append(f"- clang: {env['clang']}")
    lines.append(f"- rustc: {env['rustc']}  |  go: {env['go']}  |  zig: {env['zig']}")
    lines.append(f"- seed sha256: {env['seed_sha256_12']}  |  kernel sha256: {env['kernel_sha256_12']}"
                 f"  |  git: {env['git_rev']}")
    lines.append(f"- runs: {results['config']['runs']}  |  builds per compile sample: {results['config']['builds']}")
    lines.append("")
    lines.append("Compile time is the min of the build samples. Run time is the median of the timed "
                 "runs (one warm-up discarded); peak RSS is the max resident set across them. "
                 "Cato seed -O2 and Cato kernel -O2 share the same clang level, so that pair isolates "
                 "the two compiler implementations; seed -O3 is Cato's best for the language comparison.")
    lines.append("")

    def matrix(title, value):
        lines.append(f"## {title}")
        lines.append("")
        lines.append("| benchmark | " + " | ".join(MODE_LABEL[m] for m in active) + " |")
        lines.append("|" + "---|" * (1 + len(active)))
        for name, row in results["benchmarks"].items():
            cells = [value(row["modes"][m]) if m in row["modes"] else "—" for m in active]
            lines.append(f"| {name} | " + " | ".join(cells) + " |")
        lines.append("")

    matrix("Run time, median", lambda r: fmt_ms(r.get("median_ms")))
    matrix("Compile time, min", lambda r: fmt_ms(r.get("compile_ms")))
    matrix("Peak RSS, max (MiB)", lambda r: fmt_mib(r.get("rss_mib")))

    lines.append("## Output check")
    lines.append("")
    lines.append("| benchmark | expected | result |")
    lines.append("|-----------|----------|--------|")
    for name, row in results["benchmarks"].items():
        bad = [m for m, r in row["modes"].items() if r.get("match") is False]
        state = "all match" if not bad else "MISMATCH: " + ", ".join(bad)
        lines.append(f"| {name} | `{row['expected']}` | {state} |")
    lines.append("")
    return "\n".join(lines) + "\n"


def any_modes(results):
    s = set()
    for row in results["benchmarks"].values():
        s |= set(row["modes"].keys())
    return s


if __name__ == "__main__":
    sys.exit(main())
