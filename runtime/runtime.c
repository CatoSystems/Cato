#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
void cato_print_f64(double v) { printf("%g\n", v); }
void cato_print_i64(int64_t v) { printf("%lld\n", (long long)v); }
void cato_print_string(const char *s) { printf("%s\n", s); }

/* Phase 11: string building helpers. All returned memory is malloc'd and
   intentionally lives until process exit (the interim heap contract). */

/* Split s on sep: returns a malloc'd array of n malloc'd strings, n written
   through out_n. An empty separator or empty string yields the whole string
   as the single part. */
char **cato_str_split(const char *s, const char *sep, int64_t *out_n) {
    size_t seplen = strlen(sep);
    if (seplen == 0) {
        char **arr = malloc(sizeof(char *) * 1);
        arr[0] = strdup(s);
        *out_n = 1;
        return arr;
    }
    size_t count = 1;
    const char *p = s;
    while ((p = strstr(p, sep)) != NULL) {
        count++;
        p += seplen;
    }
    char **arr = malloc(sizeof(char *) * count);
    size_t i = 0;
    const char *start = s;
    p = s;
    while ((p = strstr(p, sep)) != NULL) {
        size_t partlen = (size_t)(p - start);
        arr[i] = malloc(partlen + 1);
        memcpy(arr[i], start, partlen);
        arr[i][partlen] = '\0';
        i++;
        p += seplen;
        start = p;
    }
    arr[i] = strdup(start);
    *out_n = (int64_t)count;
    return arr;
}

/* Join a vector of n strings with sep: one malloc'd NUL-terminated result. */
char *cato_str_join(char **arr, int64_t n, const char *sep) {
    size_t seplen = strlen(sep);
    size_t total = 0;
    for (int64_t i = 0; i < n; i++) {
        if (i > 0) total += seplen;
        total += strlen(arr[i]);
    }
    char *out = malloc(total + 1);
    char *w = out;
    for (int64_t i = 0; i < n; i++) {
        if (i > 0) {
            memcpy(w, sep, seplen);
            w += seplen;
        }
        size_t l = strlen(arr[i]);
        memcpy(w, arr[i], l);
        w += l;
    }
    *w = '\0';
    return out;
}

/* ---------------------------------------------------------------------
   Phase 15: persistent HAMT for large maps.

   A map value's header {ptr, count, cap} carries a representation tag in
   cap: 0 = flat sorted array of 16-byte (key,value) pairs (small maps,
   handled inline by generated code), 1 = ptr is a HAMT root.

   The trie is 16-way, consuming 4 mixed bits per level (16 levels cover
   the 64-bit mixed key); collisions chain in leaves. assoc copies only
   the nodes along the key's path — structural sharing, immutable
   snapshots. Nodes live until process exit: sharing makes per-node frees
   unsafe, and the ownership checker guarantees unreachable maps are
   dropped whole only via the (flat) fast path. */

typedef struct HamtLeaf {
    int64_t key;
    int64_t val;
    struct HamtLeaf *next; /* collision chain */
} HamtLeaf;

typedef struct HamtNode {
    uint32_t bitmap;   /* which of the 16 slots exist */
    /* HamtLeaf* (tagged 1) or HamtNode* (tagged 0) follow, popcount(bitmap) of them */
} HamtNode;

static uint64_t cato_mix64(uint64_t x) {
    x ^= x >> 30; x *= 0xbf58476d1ce4e5b9ULL;
    x ^= x >> 27; x *= 0x94d049bb133111ebULL;
    x ^= x >> 31;
    return x;
}

#define HAMT_IS_LEAF(p)  (((uintptr_t)(p)) & 1)
#define HAMT_LEAF(p)     ((HamtLeaf *)(((uintptr_t)(p)) & ~(uintptr_t)1))
#define HAMT_TAG_LEAF(p) ((void *)((uintptr_t)(p) | 1))

static int hamt_popcount(uint32_t b) { return __builtin_popcount(b); }

/* slot index of chunk at depth d, and the node's children array */
static HamtNode *hamt_child(HamtNode *n, int i) {
    void **children = (void **)(n + 1);
    return (HamtNode *)children[i];
}

static HamtNode *hamt_new_node(uint32_t bitmap) {
    HamtNode *n = malloc(sizeof(HamtNode) + hamt_popcount(bitmap) * sizeof(void *));
    n->bitmap = bitmap;
    return n;
}

static HamtLeaf *hamt_leaf(int64_t k, int64_t v, HamtLeaf *next) {
    HamtLeaf *l = malloc(sizeof(HamtLeaf));
    l->key = k; l->val = v; l->next = next;
    return l;
}

static const int HAMT_BITS = 4, HAMT_DEPTH = 16;

/* copy a leaf chain, replacing key's value (or inserting) */
static HamtLeaf *hamt_leaf_assoc(HamtLeaf *l, int64_t k, int64_t v) {
    if (l == NULL) return hamt_leaf(k, v, NULL);
    if (l->key == k) {
        HamtLeaf *nl = hamt_leaf(k, v, l->next);
        return nl;
    }
    return hamt_leaf(l->key, l->val, hamt_leaf_assoc(l->next, k, v));
}

static int hamt_leaf_has(HamtLeaf *l, int64_t k) {
    for (; l; l = l->next) if (l->key == k) return 1;
    return 0;
}

static void *hamt_assoc_rec(void *t, uint64_t h, int d, int64_t k, int64_t v) {
    if (t == NULL) return HAMT_TAG_LEAF(hamt_leaf(k, v, NULL));
    if (HAMT_IS_LEAF(t) || d >= HAMT_DEPTH) {
        HamtLeaf *l = HAMT_LEAF(t);
        if (d < HAMT_DEPTH && hamt_leaf_has(l, k)) {
            /* distinct keys colliding on hash bits: share nothing deeper */
        }
        return HAMT_TAG_LEAF(hamt_leaf_assoc(l, k, v));
    }
    HamtNode *n = (HamtNode *)t;
    int idx = (h >> (d * HAMT_BITS)) & 0xF;
    int bit = 1 << idx;
    void *child = NULL;
    int ci = -1;
    if (n->bitmap & bit) {
        int before = hamt_popcount(n->bitmap & (bit - 1));
        child = hamt_child(n, before);
        ci = before;
    }
    void *nchild = hamt_assoc_rec(child, h, d + 1, k, v);
    if (child == nchild) return t; /* unchanged (cannot happen: assoc always builds) */
    HamtNode *nn = hamt_new_node(n->bitmap | bit);
    int before = hamt_popcount(n->bitmap & (bit - 1));
    int cnt = hamt_popcount(n->bitmap);
    void **oc = (void **)(n + 1);
    void **nc = (void **)(nn + 1);
    int w = 0;
    for (int i = 0; i < 16; i++) {
        int b = 1 << i;
        if (nn->bitmap & b) {
            if (i == idx) nc[w] = nchild;
            else nc[w] = oc[hamt_popcount(n->bitmap & (b - 1))];
            w++;
        }
    }
    (void)ci; (void)before; (void)cnt;
    return nn;
}

void *cato_hamt_assoc(void *t, int64_t k, int64_t v) {
    return hamt_assoc_rec(t, cato_mix64((uint64_t)k), 0, k, v);
}

static int hamt_get_rec(void *t, uint64_t h, int d, int64_t k, int64_t *out) {
    if (t == NULL) return 0;
    if (HAMT_IS_LEAF(t) || d >= HAMT_DEPTH) {
        for (HamtLeaf *l = HAMT_LEAF(t); l; l = l->next)
            if (l->key == k) { *out = l->val; return 1; }
        return 0;
    }
    HamtNode *n = (HamtNode *)t;
    int idx = (h >> (d * HAMT_BITS)) & 0xF;
    if (!(n->bitmap & (1 << idx))) return 0;
    return hamt_get_rec(hamt_child(n, hamt_popcount(n->bitmap & ((1u << idx) - 1))), h, d + 1, k, out);
}

int64_t cato_hamt_get(void *t, int64_t k, int64_t def) {
    int64_t out;
    if (hamt_get_rec(t, cato_mix64((uint64_t)k), 0, k, &out)) return out;
    return def;
}

/* build a HAMT root from a flat sorted array */
void *cato_hamt_from_flat(const void *flat, int64_t len) {
    void *t = NULL;
    const int64_t *e = flat;
    for (int64_t i = 0; i < len; i++)
        t = cato_hamt_assoc(t, e[2 * i], e[2 * i + 1]);
    return t;
}
