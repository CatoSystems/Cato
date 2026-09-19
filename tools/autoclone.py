#!/usr/bin/env python3
"""autoclone.py — wrap bare non-Copy parameter uses in (clone p).

The seed's move checker rejects static reuse of an ADT-typed binding, so
the kernel's discipline is clone-at-every-use. This transform applies it
mechanically while preserving the file's text: it inserts `(clone ` and `)`
around offending tokens, working right-to-left.

Rules:
- non-Copy = every deftype in the scanned files (i64/f64/bool/str are Copy),
- a bare use of a param or non-Copy let-binding becomes (clone x) unless it
  is already a clone argument, a match scrutinee, or shadowed,
- let-binder types are inferred from the callee's return type; match-binder
  types from the variant's field types,
- comments and strings are tokenized correctly (escapes honored).

Usage: autoclone.py FILE [TYPE-SOURCE.cato ...]
"""
import re
import sys

NONCOPY_TYPES = set("""
PS Ast Asts Raws AdtTab Sigs Lams TyList Sids PList VList Defns IMap TyEnv
Decls Varnt AdtInfo Lam Sig RawDefn Param Decl Facts HExpr HExprs Binds HArms
Caps VVals Intern IRes RRes Bind2 ERes ERes1 EResk LC PVRes PVRes2 PVOne
PFRes PIARes PTyRes PArgsRes PParRes RawRes HDRRes I1Res LRes
""".split())

RET = {}      # fn name -> return type name
CTOR = {}     # constructor name -> its deftype
VARIANTS = {} # (deftype, variant) -> [field type nodes]

TOK = re.compile(r'\(|\)|\[|\]|"(?:[^"\\]|\\.)*"|;[^\n]*|[^\s()\[\];"]+')


def scan(text):
    toks = []
    for m in TOK.finditer(text):
        t = m.group(0)
        if t.startswith(';'):
            continue
        toks.append((t, m.start()))
    return toks


def parse(toks, i):
    t = toks[i]
    if t[0] in '([':
        children = []
        i += 1
        while i < len(toks) and toks[i][0] not in ')]':
            node, i = parse(toks, i)
            children.append(node)
        if i >= len(toks):
            raise IndexError("unclosed form near %s" % (toks[-1][1],))
        return ('l', t, children, toks[i]), i + 1
    return ('a', t[0], t[1]), i + 1


def parse_all(text):
    toks = scan(text)
    forms = []
    i = 0
    while i < len(toks):
        f, i = parse(toks, i)
        forms.append(f)
    return forms


def head_of(node):
    if node[0] == 'l' and node[2] and node[2][0][0] == 'a':
        return node[2][0][1]
    return None


def collect_types(forms):
    for f in forms:
        if f[0] != 'l':
            continue
        kids = f[2]
        h = kids[0][1] if kids else ''
        if h == 'deftype' and len(kids) >= 3:
            name = kids[1][1]
            CTOR[name] = name
            NONCOPY_TYPES.add(name)
            for v in kids[3:]:
                if v[0] != 'l' or not v[2]:
                    continue
                vname = v[2][0][1]
                fields = []
                if len(v[2]) > 2 and v[2][1][0] == 'l':
                    items = v[2][1][2]
                    i = 0
                    while i + 2 < len(items):
                        fields.append(items[i + 2])
                        i += 3
                VARIANTS[(name, vname)] = fields
        elif h == 'defn' and len(kids) >= 5:
            ret = kids[4]
            if ret[0] == 'a':
                RET[kids[1][1]] = ret[1]


def ty_is_noncopy(ty_node):
    if ty_node[0] == 'a':
        return ty_node[1] in NONCOPY_TYPES
    if ty_node[0] == 'l' and ty_node[2]:
        return ty_node[2][0][1] in NONCOPY_TYPES
    return False


def expr_type(node):
    if node[0] == 'l' and node[2]:
        h = node[2][0][1] if node[2][0][0] == 'a' else None
        if h in RET:
            return RET[h]
        if h in CTOR:
            return CTOR[h]
    return None


def transform(node, nc, marks, scrutinee=False):
    if node[0] == 'a':
        if node[1] in nc and not scrutinee:
            marks.add(node[2])
        return
    h = head_of(node)
    kids = node[2]
    if h == 'clone' and len(kids) == 2 and kids[1][0] == 'a':
        return
    if h == 'match':
        for k, c in enumerate(kids[1:]):
            if k == 0:
                transform(c, nc, marks, scrutinee=True)
            else:
                transform_arm(c, nc, marks)
        return
    if h == 'let':
        binds = kids[1]
        nb = set(nc)
        j = 0
        while j + 1 < len(binds[2]):
            name = binds[2][j]
            val = binds[2][j + 1]
            transform(val, nb, marks)
            nb.discard(name[1])
            bt = expr_type(val)
            if bt is not None and bt in NONCOPY_TYPES:
                nb.add(name[1])
            j += 2
        for c in kids[2:]:
            transform(c, nb, marks)
        return
    for c in kids[1:]:
        transform(c, nc, marks)


def transform_arm(arm, nc, marks):
    if arm[0] != 'l':
        return
    binders = arm[2][1] if len(arm[2]) > 1 else None
    nb = set(nc)
    vname = arm[2][0][1]
    btys = None
    for (dn, vn), tys in VARIANTS.items():
        if vn == vname:
            btys = tys
            break
    if binders is not None:
        for k, b in enumerate(binders[2]):
            if b[0] == 'a':
                nb.discard(b[1])
                if btys and k < len(btys) and ty_is_noncopy(btys[k]):
                    nb.add(b[1])
    for c in arm[2][2:]:
        transform(c, nb, marks)


def transform_defn(form, marks):
    kids = form[2]
    params = kids[2]
    nc = set()
    items = params[2]
    i = 0
    while i + 2 < len(items):
        pname_t = items[i]
        pty = items[i + 2]
        tyname = pty[1] if pty[0] == 'a' else (pty[2][0][1] if pty[2] and pty[2][0][0] == 'a' else '?')
        if tyname in NONCOPY_TYPES:
            nc.add(pname_t[1])
        i += 3
    for b in kids[4:]:
        transform(b, nc, marks)


def main(path, ret_from=()):
    for rp in ret_from:
        try:
            collect_types(parse_all(open(rp).read()))
        except (FileNotFoundError, IndexError):
            pass
    text = open(path).read()
    forms = parse_all(text)
    collect_types(forms)
    marks = set()
    for f in forms:
        if head_of(f) == 'defn':
            transform_defn(f, marks)
    for start in sorted(marks, reverse=True):
        m = TOK.match(text, start)
        te = m.end()
        text = text[:start] + "(clone " + m.group(0) + ")" + text[te:]
    sys.stdout.write(text)


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2:])
