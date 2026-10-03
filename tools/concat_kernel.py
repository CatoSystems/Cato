#!/usr/bin/env python3
"""Generate `selfhost/cato.cato` — the kernel's canonical single-namespace
concatenation (`cato build selfhost/cato.cato` → cato1).

The kernel subset has no `require` until after the fixpoint, so the
compiler's own source is one namespace: the lib, reader, hir and emit
files in canonical order, closed by `cli.cato`, which defines the only
`main`. The S5/S6/S7 test harnesses build their own concats with the
same prefix plus their `_ref` driver; this file is the distribution
form of the kernel and must be regenerated whenever any part changes
(`python3 tools/concat_kernel.py`); the bootstrap test asserts it is
fresh.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "selfhost" / "cato.cato"

# Canonical order — the S7 harness's CONCAT_FILES with the driver
# swapped: `cli.cato` replaces `emit_ref.cato` (one `main` per concat).
FILES = [
    "lib/sym",
    "lib/strbuf",
    "lib/span",
    "lib/ast",
    "lib/alist",
    "lib/result",
    "reader/read",
    "reader/dump",
    "hir/types",
    "hir/expand",
    "hir/forms",
    "hir/hexpr",
    "hir/lower",
    "hir/moveck",
    "hir/dump",
    "emit/state",
    "emit/scalars",
    "emit/heap",
    "emit/adt",
    "emit/closures",
    "emit/emit",
    "cli",
]


def main() -> None:
    parts = []
    for name in FILES:
        p = ROOT / "selfhost" / "cato" / f"{name}.cato"
        parts.append(p.read_text())
    OUT.write_text("".join(parts))
    print(f"wrote {OUT.relative_to(ROOT)} ({sum(len(p) for p in parts)} bytes)")


if __name__ == "__main__":
    main()
