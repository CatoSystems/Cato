#!/usr/bin/env python3
"""instr_clones.py — instrument clone/drop call sites in an emitted kernel IR.

Usage: instr_clones.py IN.ll OUT.ll MAP.tsv

Every `call ... @clone__T(...)` is bracketed with
`__dbg_enter(ID)` / `__dbg_exit(ID)`, so tools/dbg_hit.c can attribute every
allocation made *inside* the call (including the whole recursive clone) to
that one call site. `drop__` calls get a plain `__dbg_hit(ID)` counter.

MAP.tsv records `ID<TAB>enclosing-function<TAB>callee`, and the instrumented
binary prints `SITE<TAB>ID<TAB>calls<TAB>bytes<TAB>fn<TAB>callee` at exit.
"""
import re
import sys

CLONE = re.compile(r"^\s*(?:%\S+\s*=\s*)?call\s+[^@]*@(clone__\w+)\(")
DROP = re.compile(r"^\s*(?:%\S+\s*=\s*)?call\s+[^@]*@(drop__\w+)\(")
DEF = re.compile(r"^define\s+[^@]*@([A-Za-z0-9_.$]+)\(")


def main() -> None:
    src, out, mapp = sys.argv[1], sys.argv[2], sys.argv[3]
    lines = open(src).read().split("\n")
    res, mapping = [], {}
    nid, fn = 0, "?"
    for ln in lines:
        m = DEF.match(ln)
        if m:
            fn = m.group(1)
        c = CLONE.match(ln)
        if c:
            nid += 1
            mapping[nid] = (fn, c.group(1), "clone")
            res.append("  call void @__dbg_enter(i32 %d)" % nid)
            res.append(ln)
            res.append("  call void @__dbg_exit(i32 %d)" % nid)
            continue
        d = DROP.match(ln)
        if d:
            nid += 1
            mapping[nid] = (fn, d.group(1), "drop")
            res.append("  call void @__dbg_hit(i32 %d)" % nid)
        res.append(ln)
    for i, ln in enumerate(res):
        if ln.startswith("define "):
            res.insert(
                i,
                "declare void @__dbg_enter(i32)\n"
                "declare void @__dbg_exit(i32)\n"
                "declare void @__dbg_hit(i32)\n",
            )
            break
    open(out, "w").write("\n".join(res))
    with open(mapp, "w") as f:
        for k in sorted(mapping):
            f.write("%d\t%s\t%s\n" % (k, mapping[k][0], mapping[k][1]))
    print("instrumented %d call sites (%d bracketed clones)"
          % (nid, sum(1 for v in mapping.values() if v[2] == "clone")))


if __name__ == "__main__":
    main()
