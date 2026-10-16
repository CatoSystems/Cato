#!/usr/bin/env python3
"""borrow_drop.py — drop a `(clone X)` argument that a `&T` parameter never
consumes.

A callee that declares `p :- &T` only borrows its argument: the caller's
binding stays valid, so wrapping the argument in `(clone X)` is pure waste —
and for the kernel's table types (AdtTab, Sids, Sigs, HExpr, ...) a clone is
a *deep* copy of the whole structure, so these sites are where most of the
kernel's allocation churn is born.

This is the mechanical half of the borrow conversion: signatures are a
semantic decision (done by hand), argument clones are not.

Usage: borrow_drop.py FILE [TYPE-SOURCE.cato ...] > FILE.new

The whole kernel's sources must be passed as type sources so cross-module
signatures resolve; the first FILE is the one rewritten.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import autoclone as _ac


def is_atom(n):
    return n[0] == "a"


def head(node):
    if node[0] == "l" and node[2] and node[2][0][0] == "a":
        return node[2][0][1]
    return None


def ty_borrowed(ty_node):
    """True iff a parameter's declared type is a borrow (`&T`)."""
    if ty_node is None:
        return False
    if ty_node[0] == "a":
        return ty_node[1].startswith("&")
    if ty_node[0] == "l" and ty_node[2] and ty_node[2][0][0] == "a":
        return ty_node[2][0][1].startswith("&")
    return False


def clone_arg(node):
    """Return the atom argument of a `(clone X)` form, else None."""
    if node[0] == "l":
        k = node[2]
        if len(k) == 2 and k[0][0] == "a" and k[0][1] == "clone" and k[1][0] == "a":
            return k[1]
    return None


def collect_signatures(forms, sigs):
    for f in forms:
        if head(f) not in ("defn",):
            continue
        k = f[2]
        if len(k) < 3 or k[1][0] != "a" or k[2][0] != "l":
            continue
        name = k[1][1]
        items = k[2][2]
        params = []
        i = 0
        while i + 2 < len(items):
            params.append(ty_borrowed(items[i + 2]))
            i += 3
        sigs[name] = params


def walk(node, sigs, edits):
    if node[0] != "l":
        return
    h = head(node)
    k = node[2]
    if h == "defn":
        # Bodies of nested defns are walked too (they are calls like any
        # other); only the parameter list is skipped.
        for i, c in enumerate(k):
            if i == 2:
                continue
            walk(c, sigs, edits)
        return
    if h is not None and h in sigs:
        params = sigs[h]
        args = k[1:]
        if len(args) == len(params):
            for pos, (arg, borrowed) in enumerate(zip(args, params)):
                if borrowed:
                    a = clone_arg(arg)
                    if a is not None:
                        edits.append((arg[1][1], arg[3][1] + 1, a[1]))
    for c in k[1:]:
        walk(c, sigs, edits)


def main():
    if len(sys.argv) < 2:
        sys.stderr.write("usage: borrow_drop.py FILE [TYPE-SOURCE.cato ...]\n")
        sys.exit(2)
    sigs = {}
    sources = sys.argv[2:]
    for rp in sources:
        try:
            collect_signatures(_ac.parse_all(open(rp).read()), sigs)
        except (FileNotFoundError, IndexError):
            pass
    text = open(sys.argv[1]).read()
    forms = _ac.parse_all(text)
    collect_signatures(forms, sigs)
    edits = []
    for f in forms:
        walk(f, sigs, edits)
    for start, end, repl in sorted(edits, reverse=True):
        text = text[:start] + repl + text[end:]
    sys.stderr.write("borrow_drop: removed %d clone(s)\n" % len(edits))
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
