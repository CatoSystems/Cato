# Cato benchmarks

Fair, reproducible comparison of Cato against Rust, Go and Zig, and of the two
Cato compiler implementations (the Rust **seed** and the self-hosted **kernel**).

```
./scripts/run_all.sh                 # full run (3 builds, 7 runs per binary)
./scripts/run_all.sh --quick         # fast sanity run (1 build, 3 runs)
./scripts/run_all.sh --only tree,pipe
```

`run_all.sh` builds the release seed and the kernel (`cato1`), then runs
`scripts/bench.py`, which prints the report and writes
`results/run_<timestamp>.md` + `.json`.

## Programs

Each program is implemented once per language with the **same algorithm and the
same workload**; every implementation prints one line that must match exactly.

| program | tier | what it stresses |
|---|---|---|
| `fib_rec` | shared | recursive calls (`fib 33`) |
| `fib_iter` | shared | tight integer loop: `fibmod(100000000, 1000000007)` |
| `str_bytes` | shared | string indexing, inner loop |
| `vec_append` | shared | growable vector `conj` in a loop (N=20000) |
| `tree` | shared | ADT allocation + recursive traversal (100000 nodes) |
| `mandelbrot` | shared | floating point (400x400) |
| `fused_sum` | full | seed-only: `reduce`/`range` stream fusion |
| `pipe` | full | seed-only: `filter`/`map` pipeline fusion |

* **shared** — the same Cato source compiles under both the seed and the kernel.
* **full** — uses macros the kernel does not implement yet, so it is compared
  seed vs Rust/Go/Zig only.

`vec_append` is capped at N=20000 on purpose: the kernel's in-loop `conj` is
super-linear (see *Known kernel issues*), so larger inputs would just measure
that pathology rather than collection performance.

## Fairness rules

* **Same clang level for the seed↔kernel head-to-head.** The kernel CLI pins
  `clang -O2` (`selfhost/cato/cli.cato`), so the kernel column is always `-O2`,
  and `cato-seed-o2` uses the same level — that pair isolates the two compiler
  implementations. `cato-seed-o3` is also reported as Cato's best for the
  language-vs-language table.
* **Each language at its standard release settings.** Rust `rustc -C
  opt-level=3`, Go `go build`, Zig `zig build-exe -O ReleaseFast -lc`.
* **Compile time is end-to-end**: source → native binary, including the C/link
  step, for every language. The minimum of `--builds` builds is reported (it
  filters scheduler noise; each build starts from no output artifact).
* **Run time** is the median of `--runs` runs; one warm-up run is discarded.
* **Peak RSS** is the maximum resident set over the timed runs. On macOS
  `ru_maxrss` is bytes, on Linux KiB; the harness normalizes to MiB.
* Workloads are sized so compute dominates process startup.

## Output layout

The report contains a machine/toolchain header (with the seed and kernel
`sha256` and the git revision) and three matrices — run time, compile time,
peak RSS — plus an output-check table. The `.json` file holds the same data.

`—` means a mode does not apply (kernel on a `full` program) or its tool is
missing.

## Known kernel issues (surfaced by these benchmarks)

* **`conj` in a loop** was super-linear (kernel ~217 ms / 1697 MiB at N=20000,
  OOM at 200k). Fixed by the loop-conj capacity port (recur carries the grow
  slot whole); vec_append is at parity in the 2026-10-18 runs.
* The kernel build CLI historically exited `0` even when `cc` failed; the
  harness verifies the output artifact exists and is non-empty.
* A `defn` named `even?` breaks the seed build (invalid LLVM symbol), so
  programs avoid `?` in names.
