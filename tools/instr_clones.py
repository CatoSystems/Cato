#!/usr/bin/env python3
"""instr_clones.py — insert per-call-site counters before every clone/drop
call in an emitted kernel IR file.

Usage: instr_clones.py IN.ll OUT.ll MAP.tsv

For each `call ... @clone__T(...)` / `@drop__T(...)` the script emits a
`call void @__dbg_hit(i32 ID)` immediately before it and records
`ID<TAB>enclosing-function<TAB>callee<TAB>ir-line` in MAP.tsv. Running the
instrumented binary prints "ID count" rows at exit (see tools/dbg_hit.c),
which turns allocation churn into an exact per-call-site table.
"""
import re
import sys

CALL = re.compile(r"^\s*(?:%\S+\s*=\s*)?call\s+[^@]*@((?:clone|drop)__\w+)\(")
DEF = re.compile(r"^define\s+[^@]*@([A-Za-z0-9_.$]+)\(")


def main() -> None:
    src, out, mapp = sys.argv[1], sys.argv[2], sys.argv[3]
    lines = open(src).read().split("\n")
    res = []
    mapping = {}
    nid = 0
    fn = "?"
    for ln in lines:
        m = DEF.match(ln)
        if m:
            fn = m.group(1)
        c = CALL.match(ln)
        if c:
            nid += 1
            mapping[nid] = (fn, c.group(1), ln.strip()[:110])
            res.append("  call void @__dbg_hit(i32 %d)" % nid)
        res.append(ln)
    # Declare the hook once, right after the module header.
    for i, ln in enumerate(res):
        if ln.startswith("define "):
            res.insert(i, "declare void @__dbg_hit(i32)\n")
            break
    open(out, "w").write("\n".join(res))
    with open(mapp, "w") as f:
        for k in sorted(mapping):
            f.write("%d\t%s\t%s\t%s\n" % (k, *mapping[k]))
    print("instrumented %d call sites" % nid)


if __name__ == "__main__":
    main()
