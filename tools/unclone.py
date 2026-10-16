#!/usr/bin/env python3
"""unclone.py — remove redundant (clone X) calls from Cato source.

The inverse of tools/autoclone.py. autoclone wraps every non-Copy use in
(clone X) because the seed's move checker rejects static reuse of an owned
binding; that over-clones badly (the kernel's self-build spends most of its
allocation churn in clone__HExpr et al). The checker only requires a clone
when a value is reused on a path: a value may be *moved* once. This transform
unwraps (clone X) at the last use of X on every control-flow path.

Semantics (verified against the seed checker):
- match arms and if/cond branches are alternative paths, so the same binder
  may move in each of them;
- straight-line sequences (call args, do bodies, let value -> let body) share
  one path, so only the last use moves and earlier uses stay cloned;
- Copy types (i64/str/bool/f64/...) are never touched.

Parsing and the type/scope machinery are reused from autoclone.py.

Usage: unclone.py FILE [TYPE-SOURCE.cato ...]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import autoclone as _ac

COPY_TYPES = {
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64",
    "f32", "f64", "bool", "str", "char", "unit", "void", "()",
}


class Binder:
    __slots__ = ("name", "eligible", "uses", "bare")

    def __init__(self, name, eligible):
        self.name = name
        self.eligible = eligible
        self.uses = []
        self.bare = False  # referenced as a bare atom (a potential move)


class Scope:
    __slots__ = ("parent", "binders", "no_move")

    def __init__(self, parent):
        self.parent = parent
        self.binders = {}
        self.no_move = set()

    def lookup(self, name):
        s = self
        while s is not None:
            b = s.binders.get(name)
            if b is not None:
                return b
            s = s.parent
        return None

    def is_no_move(self, name):
        s = self
        while s is not None:
            if name in s.no_move:
                return True
            s = s.parent
        return False


def is_atom(n):
    return n[0] == "a"


def is_list(n):
    return n[0] == "l"


def kids(n):
    return n[2]


def head(n):
    if n[0] == "l" and n[2] and n[2][0][0] == "a":
        return n[2][0][1]
    return None


def clone_arg(n):
    """Return the atom argument of a (clone X) form, else None."""
    if n[0] == "l":
        k = n[2]
        if len(k) == 2 and k[0][0] == "a" and k[0][1] == "clone" and k[1][0] == "a":
            return k[1]
    return None


def ty_copy(ty_node):
    """True iff the type annotation is a known Copy primitive."""
    if ty_node is None:
        return False
    if ty_node[0] == "a":
        return ty_node[1] in COPY_TYPES
    if ty_node[0] == "l" and ty_node[2] and ty_node[2][0][0] == "a":
        return False  # (Vector T) and friends own memory
    return False


def ty_borrowed(ty_node):
    """True iff the type annotation is a borrow (`&T`)."""
    if ty_node is None:
        return False
    if ty_node[0] == "a":
        return ty_node[1].startswith("&")
    if ty_node[0] == "l" and ty_node[2]:
        h = ty_node[2][0]
        return h[0] == "a" and h[1].startswith("&")
    return False


def bind_params(defn, scope):
    params = defn[2][2]
    items = params[2]
    i = 0
    while i + 2 < len(items):
        name = items[i][1]
        ty = items[i + 2]
        if ty_borrowed(ty):
            scope.no_move.add(name)
        scope.binders[name] = Binder(name, not ty_copy(ty) and not ty_borrowed(ty))
        i += 3


def bind_arm(arm, scope):
    if arm[0] != "l" or len(arm[2]) < 2:
        return
    binders = arm[2][1]
    if binders[0] != "l":
        return
    vname = arm[2][0][1] if arm[2][0][0] == "a" else None
    btys = None
    for (dn, vn), tys in _ac.VARIANTS.items():
        if vn == vname:
            btys = tys
            break
    for k, b in enumerate(binders[2]):
        if b[0] != "a":
            continue
        if btys is not None and k < len(btys):
            eligible = not ty_copy(btys[k])
        else:
            eligible = True
        scope.binders[b[1]] = Binder(b[1], eligible)


def bind_let(binds, scope):
    """Bind let names into scope, returning [(name, value_node, binder)]."""
    out = []
    items = binds[2]
    j = 0
    while j + 1 < len(items):
        name = items[j][1]
        val = items[j + 1]
        b = Binder(name, True)  # refined by caller from the value's type
        scope.binders[name] = b
        out.append((name, val, b))
        j += 2
    return out


def collect_uses(node, scope, inner, mark=True):
    """Set of outer Binder objects referenced in node (inner ones excluded).

    `mark` is False for positions that are never moves (a match scrutinee, an
    if condition): a bare atom there borrows, so it must not suppress the
    last-use move of an arm binder / clone elsewhere on the path.
    """
    if is_atom(node):
        b = scope.lookup(node[1])
        if b is not None and b not in inner:
            if mark:
                b.bare = True  # a bare atom use is a potential move
            return {b}
        return set()
    if not is_list(node):
        return set()
    h = head(node)
    k = node[2]
    if clone_arg(node) is not None:
        b = scope.lookup(clone_arg(node)[1])
        if b is not None and b not in inner:
            return {b}
        return set()
    if h == "defn":
        return set()  # nested defn: separate function, ignore
    if h == "match":
        res = collect_uses(k[1], scope, inner, mark=False)
        for arm in k[2:]:
            ns = Scope(scope)
            bind_arm(arm, ns)
            for n in ns.binders.values():
                inner.add(n)
            for body in arm[2][2:]:
                res |= collect_uses(body, ns, inner, mark)
        return res
    if h == "cond":
        res = set()
        pairs = k[1:]
        for i in range(0, len(pairs), 2):
            res |= collect_uses(pairs[i], scope, inner, mark=False)
            if i + 1 < len(pairs):
                res |= collect_uses(pairs[i + 1], scope, inner, mark)
        return res
    if h == "if":
        res = collect_uses(k[1], scope, inner, mark=False)
        for c in k[2:]:
            res |= collect_uses(c, scope, inner, mark)
        return res
    if h == "let":
        ns = Scope(scope)
        vals = bind_let(k[1], ns)
        for n in ns.binders.values():
            inner.add(n)
        res = set()
        for _, val, _ in vals:
            res |= collect_uses(val, ns, inner, mark)
        for body in k[2:]:
            res |= collect_uses(body, ns, inner, mark)
        return res
    res = set()
    for c in k[1:]:
        res |= collect_uses(c, scope, inner, mark)
    return res


def uses_of(node, scope):
    return collect_uses(node, scope, set())


def walk(node, scope, after, edits, mark=True):
    if not is_list(node):
        if is_atom(node):
            b = scope.lookup(node[1])
            if b is not None and mark:
                b.bare = True
        return
    h = head(node)
    k = node[2]
    arg = clone_arg(node)
    if arg is not None:
        b = scope.lookup(arg[1])
        if b is not None and b.eligible and not b.bare and b not in after:
            edits.append((node[1][1], node[3][1] + 1, arg[1]))
        return
    if h == "defn":
        return
    if h == "match":
        arms = k[2:]
        sel = k[1]
        sel_var = sel[1] if is_atom(sel) else None
        sel_no_move = sel_var is not None and scope.is_no_move(sel_var)
        sb = scope.lookup(sel_var) if sel_var is not None else None
        sel_used_after = sb is not None and sb in after
        # First pass: gather what the arms reference (outer binders), before
        # we decide eligibility. Track whether any arm's *own* binder is used.
        arm_uses = set()
        arm_binders_used = False
        for arm in arms:
            ns = Scope(scope)
            bind_arm(arm, ns)
            own = set(ns.binders.values())
            for body in arm[2][2:]:
                u = uses_of(body, ns)
                arm_uses |= u
                if own & u:
                    arm_binders_used = True
        # Moving an arm binder of a bare-Var scrutinee retires the scrutinee
        # (the checker's box watch). If the scrutinee is *also* referenced in
        # an arm — e.g. `match ps2 (PS [ … y3 … ] (PTyRes ps2 (f y3)))` moves
        # both the binder and the owner — that's a double move: keep the
        # binder clones. `sel_used_after` covers the scrutinee living past the
        # match; this covers it being moved inside an arm.
        sel_used_in_arms = sb is not None and sb in arm_uses
        protect = sel_no_move or sel_used_after or sel_used_in_arms
        # Never move the owner of a destructured box when one of its field
        # binders is live. `(PTyRes ps2 (f (clone y3)))` — if `ps2` is moved at
        # arg0 its box is retired before `y3` (a field alias) is read at arg1 →
        # "use of moved value". Keeping the scrutinee's clone is always legal;
        # the arm-binder clones (the real win) are still elided below. When no
        # field binder is used the scrutinee may still move (see
        # test_same_binder_moves_in_each_arm).
        if clone_arg(sel) is None and sb is not None and arm_binders_used:
            sb.eligible = False
        # Keep the scrutinee's own clone. `(match (clone r) …)` gives the
        # checker a temp scrutinee; a bare Var scrutinee plus a moved arm
        # binder trips its box watch/retirement model ("use of moved
        # value"). The temp clone is load-bearing; the arm-binder clones
        # (the expensive ones) are still elided.
        if clone_arg(sel) is None:
            walk(sel, scope, after | arm_uses, edits, mark=False)
        for arm in arms:
            ns = Scope(scope)
            bind_arm(arm, ns)
            if protect:
                for b in ns.binders.values():
                    b.eligible = False
                    if sel_no_move:
                        ns.no_move.add(b.name)
            walk_block(arm[2][2:], ns, after, edits)
        return
    if h == "cond":
        # `(cond t1 e1 t2 e2 ...)` is an if-chain: the tests are evaluated in
        # order, but the result expressions are alternative paths, so a binder
        # may be moved in each of them (and must stay cloned only when a later
        # test or result still reads it).
        pairs = k[1:]
        for i in range(0, len(pairs), 2):
            test = pairs[i]
            res = pairs[i + 1] if i + 1 < len(pairs) else None
            later = set()
            for j in range(i + 1, len(pairs)):
                later |= uses_of(pairs[j], scope)
            walk(test, scope, after | later, edits, mark=False)
            if res is not None:
                walk(res, scope, after | later, edits)
        return
    if h == "if":
        cond = k[1]
        then = k[2] if len(k) > 2 else None
        els = k[3] if len(k) > 3 else None
        other = set()
        if then is not None:
            other |= uses_of(then, scope)
        if els is not None:
            other |= uses_of(els, scope)
        walk(cond, scope, after | other, edits, mark=False)
        if then is not None:
            walk(then, scope, after, edits)
        if els is not None:
            walk(els, scope, after, edits)
        return
    if h == "let":
        ns = Scope(scope)
        vals = bind_let(k[1], ns)
        for name, val, b in vals:
            bt = _ac.expr_type(val)
            b.eligible = bt is None or not ty_copy_atom(bt)
        body_uses = set()
        for body in k[2:]:
            body_uses |= uses_of(body, ns)
        n = len(vals)
        for i, (name, val, b) in enumerate(vals):
            later = set(body_uses)
            for _, v2, _ in vals[i + 1:]:
                later |= uses_of(v2, ns)
            walk(val, ns, after | later, edits)
        walk_block(k[2:], ns, after, edits)
        return
    walk_block(k[1:], scope, after, edits)


def ty_copy_atom(name):
    return name in COPY_TYPES


def walk_block(exprs, scope, after, edits):
    n = len(exprs)
    for i, e in enumerate(exprs):
        later = set()
        for j in range(i + 1, n):
            later |= uses_of(exprs[j], scope)
        walk(e, scope, after | later, edits)


def unclone_text(text, type_sources):
    for rp in type_sources:
        try:
            _ac.collect_types(_ac.parse_all(open(rp).read()))
        except (FileNotFoundError, IndexError):
            pass
    forms = _ac.parse_all(text)
    _ac.collect_types(forms)
    edits = []
    for f in forms:
        if head(f) == "defn":
            scope = Scope(None)
            bind_params(f, scope)
            walk_block(f[2][4:], scope, set(), edits)
    for start, end, repl in sorted(edits, reverse=True):
        text = text[:start] + repl + text[end:]
    return text, len(edits)


def main():
    if len(sys.argv) < 2:
        sys.stderr.write("usage: unclone.py FILE [TYPE-SOURCE.cato ...]\n")
        sys.exit(2)
    text, n = unclone_text(open(sys.argv[1]).read(), sys.argv[2:])
    sys.stderr.write("unclone: removed %d clone(s)\n" % n)
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
