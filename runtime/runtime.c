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
    size_t rc;         /* reference count: referencing edges (starts at 1) */
    int64_t key;
    int64_t val;
    struct HamtLeaf *next; /* collision chain */
} HamtLeaf;

typedef struct HamtNode {
    size_t rc;
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
    n->rc = 1;
    n->bitmap = bitmap;
    return n;
}

static HamtLeaf *hamt_leaf(int64_t k, int64_t v, HamtLeaf *next) {
    HamtLeaf *l = malloc(sizeof(HamtLeaf));
    l->rc = 1;
    l->key = k; l->val = v; l->next = next;
    return l;
}

/* retain/release: a shared node's count is the number of referencing
   edges; at zero the node frees and the release recurses. */
static void hamt_release_rec(void *t);

static void hamt_retain(void *t) {
    if (t == NULL) return;
    if (HAMT_IS_LEAF(t)) HAMT_LEAF(t)->rc++;
    else ((HamtNode *)t)->rc++;
}

static void hamt_release_rec(void *t) {
    if (t == NULL) return;
    if (HAMT_IS_LEAF(t)) {
        HamtLeaf *l = HAMT_LEAF(t);
        if (--l->rc == 0) {
            hamt_release_rec(l->next ? HAMT_TAG_LEAF(l->next) : NULL);
            free(l);
        }
        return;
    }
    HamtNode *n = (HamtNode *)t;
    if (--n->rc != 0) return;
    void **children = (void **)(n + 1);
    int cnt = hamt_popcount(n->bitmap);
    for (int i = 0; i < cnt; i++) hamt_release_rec(children[i]);
    free(n);
}

static const int HAMT_BITS = 4, HAMT_DEPTH = 16;

/* copy a leaf chain, replacing key's value (or inserting) */
static HamtLeaf *hamt_leaf_assoc(HamtLeaf *l, int64_t k, int64_t v) {
    if (l == NULL) return hamt_leaf(k, v, NULL);
    if (l->key == k) {
        /* the replacement shares the old tail chain */
        if (l->next) hamt_retain(HAMT_TAG_LEAF(l->next));
        return hamt_leaf(k, v, l->next);
    }
    /* fresh copies all the way down: nothing shared */
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
            else {
                void *shared = oc[hamt_popcount(n->bitmap & (b - 1))];
                hamt_retain(shared); /* the new root references it too */
                nc[w] = shared;
            }
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

/* Find with presence report: writes 1/0 to *found and returns the value
   (0 on miss). Generated code uses this for Option-returning map gets. */
int64_t cato_hamt_find(void *t, int64_t k, void *found_out) {
    int64_t out;
    int f = hamt_get_rec(t, cato_mix64((uint64_t)k), 0, k, &out);
    *(int64_t *)found_out = f;
    return out;
}

/* (Vector str) ownership: a string vector owns a heap copy of every
   element string. conj strdups the appended value; a persistent copy
   strdups all live elements (each vector owns its own); release frees
   each element, then the array. */
char **cato_str_vec_copy(char **src, int64_t len, int64_t cap) {
    if (cap < 1) cap = 1;  /* empty copies still get a real address */
    char **out = malloc(cap * sizeof(char *));
    for (int64_t i = 0; i < len; i++) out[i] = strdup(src[i]);
    return out;
}

void cato_str_vec_free(char **arr, int64_t len) {
    for (int64_t i = 0; i < len; i++) free(arr[i]);
    free(arr);
}

/* Symbol table: an append-only intern table behind an opaque i64 handle.
   The kernel threads the handle (cloning it is O(1)); the table itself is
   shared across every snapshot and never copied or freed, so a compiler
   state copy no longer deep-copies the whole table. Entries are owned by
   the table and live until exit. */
typedef struct { int64_t len; int64_t cap; char **items; } CatoStab;

int64_t cato_stab_new(void) {
    CatoStab *s = malloc(sizeof(CatoStab));
    s->len = 0;
    s->cap = 8;
    s->items = malloc(sizeof(char *) * (size_t)s->cap);
    return (int64_t)(intptr_t)s;
}

int64_t cato_stab_count(int64_t h) {
    CatoStab *s = (CatoStab *)(intptr_t)h;
    return s ? s->len : 0;
}

/* Owned copy of entry i (callers treat the result as owned); "" if the
   index is out of range. */
char *cato_stab_get(int64_t h, int64_t i) {
    CatoStab *s = (CatoStab *)(intptr_t)h;
    if (!s || i < 0 || i >= s->len) return strdup("");
    return strdup(s->items[i]);
}

/* Back-to-front scan; misses report -1 (matches intern-find). */
int64_t cato_stab_find(int64_t h, const char *v) {
    CatoStab *s = (CatoStab *)(intptr_t)h;
    if (!s) return -1;
    for (int64_t i = s->len - 1; i >= 0; i--)
        if (strcmp(s->items[i], v) == 0) return i;
    return -1;
}

/* Append (unconditionally) and return the new sid. */
int64_t cato_stab_conj(int64_t h, const char *v) {
    CatoStab *s = (CatoStab *)(intptr_t)h;
    if (s->len == s->cap) {
        s->cap *= 2;
        s->items = realloc(s->items, sizeof(char *) * (size_t)s->cap);
    }
    s->items[s->len] = strdup(v);
    return s->len++;
}

/* Append and return the (unchanged) handle — for builder chains that
   must keep threading the table rather than the sid. */
int64_t cato_stab_add(int64_t h, const char *v) {
    cato_stab_conj(h, v);
    return h;
}

/* Release a map's heap: `cap` is the header's cap word (tag bit 1 = HAMT
   root, 0 = flat sorted array). Called by generated drop glue. */
void cato_map_release_pair(void *root, int64_t cap) {
    if ((cap & 1) == 0) { free(root); return; }
    hamt_release_rec(root);
}

/* build a HAMT root from a flat sorted array */
void *cato_hamt_from_flat(const void *flat, int64_t len) {
    void *t = NULL;
    const int64_t *e = flat;
    for (int64_t i = 0; i < len; i++)
        t = cato_hamt_assoc(t, e[2 * i], e[2 * i + 1]);
    return t;
}

// ---------------------------------------------------------------------------
// S3: host surface
// ---------------------------------------------------------------------------

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

void cato_print_str_raw(const char *s) { fputs(s, stdout); }

/* Unchecked byte read, `(cato-str-byte s i)`. The language's `(nth s i d)`
   is bounds-checked, which costs a strlen per access; byte-at-a-time walks
   (the kernel reader's scan over its own source) therefore pay a strlen per
   byte — quadratic. Callers that already carry the string length and guard
   the index (as every reader walk does) use this instead. Reading one past
   the end is defined here and yields the NUL terminator, matching the
   `(nth s i 0)` default of 0. */
int64_t cato_str_byte(const char *s, int64_t i) { return (unsigned char)s[i]; }

char *cato_num_to_str(long long v) {
    char buf[32];
    snprintf(buf, sizeof buf, "%lld", v);
    char *out = malloc(strlen(buf) + 1);
    strcpy(out, buf);
    return out;
}

char *cato_u64_to_str(unsigned long long v) {
    char buf[32];
    snprintf(buf, sizeof buf, "%llu", v);
    char *out = malloc(strlen(buf) + 1);
    strcpy(out, buf);
    return out;
}

char *cato_f64_to_str(double v) {
    char buf[64];
    snprintf(buf, sizeof buf, "%g", v);
    char *out = malloc(strlen(buf) + 1);
    strcpy(out, buf);
    return out;
}

// A heap `(Option i64)` / `(Option f64)` box: { i64 tag, i64 payload }.
// Tag 0 = Some, 1 = None (variant declaration order).
void *cato_parse_i64(const char *s) {
    char *end = NULL;
    errno = 0;
    long long v = strtoll(s, &end, 10);
    // Overflow (ERANGE) is None, matching the interpreter's parse.
    int bad = !end || end == s || *end != '\0' || errno == ERANGE;
    void *box = malloc(16);
    long long *words = (long long *)box;
    if (bad) {
        words[0] = 1;
        words[1] = 0;
    } else {
        words[0] = 0;
        words[1] = v;
    }
    return box;
}

void *cato_parse_f64(const char *s) {
    char *end = NULL;
    double v = strtod(s, &end);
    void *box = malloc(16);
    long long *words = (long long *)box;
    if (!end || end == s || *end != '\0') {
        words[0] = 1;
        words[1] = 0;
    } else {
        words[0] = 0;
        memcpy(&words[1], &v, sizeof v);
    }
    return box;
}

char *cato_slurp(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        char *empty = malloc(1);
        empty[0] = '\0';
        return empty;
    }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *out = malloc((size_t)n + 1);
    size_t got = fread(out, 1, (size_t)n, f);
    out[got] = '\0';
    fclose(f);
    return out;
}

void cato_spit(const char *path, const char *content) {
    FILE *f = fopen(path, "wb");
    if (!f) return;
    fwrite(content, 1, strlen(content), f);
    fclose(f);
}

static char **g_argv_values = NULL;
static long long g_argc = 0;

void cato_args_init(int argc, char **argv) {
    if (argc > 1) {
        g_argv_values = malloc(sizeof(char *) * (size_t)(argc - 1));
        for (int i = 1; i < argc; i++) {
            g_argv_values[i - 1] = strdup(argv[i]);
        }
        g_argc = argc - 1;
    }
}

// Returns the argument data pointer; stores the length through out_n.
char **cato_args(long long *out_n) {
    if (g_argv_values == NULL) {
        g_argv_values = malloc(sizeof(char *));
        g_argv_values[0] = strdup("");
        g_argc = 1;
        *out_n = 0;
        return g_argv_values;
    }
    *out_n = g_argc;
    return g_argv_values;
}

void cato_exit(long long status) { exit((int)status); }

/* Integer division, rem or mod by zero: a defined trap (the interpreter
   traps too), never LLVM's undefined `sdiv` by zero. */
void cato_div_by_zero(void) {
    fputs("cato: integer division by zero\n", stderr);
    abort();
}

// ---------------------------------------------------------------------------
// Kernel CLI primitives: process spawn, environment, files, stdio.
// runtime.c links into every Cato program, so the POSIX process calls are
// guarded: Windows and wasm32 builds get stubs (spawn reports -1).
// ---------------------------------------------------------------------------
#include <stdbool.h>
#if defined(_WIN32)
#include <io.h>
#define cato_access _access
#elif !defined(__wasm__)
#include <unistd.h>
#include <sys/wait.h>
#define cato_access access
#endif

// Run argv[0] with argv (PATH lookup, no shell) and wait. Returns the exit
// status; 128+signal when killed; 127 when the program cannot be executed;
// -1 when no process could be started (or on unsupported platforms).
int64_t cato_spawn(char **argv_data, int64_t n) {
#if defined(_WIN32) || defined(__wasm__)
    (void)argv_data; (void)n;
    return -1;
#else
    if (n <= 0) return -1;
    char **argv = (char **)malloc(sizeof(char *) * ((size_t)n + 1));
    if (!argv) return -1;
    for (int64_t i = 0; i < n; i++) argv[i] = argv_data[i];
    argv[n] = NULL;
    fflush(stdout);
    fflush(stderr);
    pid_t pid = fork();
    if (pid < 0) { free(argv); return -1; }
    if (pid == 0) {
        execvp(argv[0], argv);
        _exit(127);
    }
    int status = 0;
    pid_t w;
    do { w = waitpid(pid, &status, 0); } while (w < 0 && errno == EINTR);
    free(argv);
    if (w < 0) return -1;
    if (WIFEXITED(status)) return (int64_t)WEXITSTATUS(status);
    if (WIFSIGNALED(status)) return (int64_t)(128 + WTERMSIG(status));
    return -1;
#endif
}

static char *cato_dup_or_empty(const char *s) {
    char *r = strdup(s ? s : "");
    if (!r) { fputs("cato: out of memory\n", stderr); exit(1); }
    return r;
}

// The variable's value, or "" when unset.
char *cato_getenv(const char *name) {
#if defined(__wasm__)
    (void)name;
    return cato_dup_or_empty("");
#else
    return cato_dup_or_empty(getenv(name));
#endif
}

bool cato_file_exists(const char *path) {
#if defined(__wasm__)
    (void)path;
    return false;
#else
    return cato_access(path, 0) == 0;
#endif
}

// 0 on success, -1 on failure.
int64_t cato_remove_file(const char *path) {
    return remove(path) == 0 ? 0 : -1;
}

// One line from stdin without its "\n" / "\r\n"; "" at end of input (an
// empty line and EOF are indistinguishable by design of the API).
char *stdin_getline(void) {
    size_t cap = 128, n = 0;
    char *line = malloc(cap);
    if (!line) { fputs("cato: out of memory\n", stderr); exit(1); }
    int c;
    while ((c = fgetc(stdin)) != EOF && c != '\n') {
        if (n + 1 >= cap) {
            cap *= 2;
            char *grown = realloc(line, cap);
            if (!grown) { free(line); fputs("cato: out of memory\n", stderr); exit(1); }
            line = grown;
        }
        line[n++] = (char)c;
    }
    if (n > 0 && line[n - 1] == '\r') n--;
    line[n] = '\0';
    return line;
}

char *stdin_read_exact(int64_t n) {
    if (n < 0) n = 0;
    char *buf = malloc((size_t)n + 1);
    if (!buf) { fputs("cato: out of memory\n", stderr); exit(1); }
    size_t got = 0;
    while (got < (size_t)n) {
        size_t r = fread(buf + got, 1, (size_t)n - got, stdin);
        if (r == 0) break;
        got += r;
    }
    buf[got] = '\0';
    return buf;
}

int64_t stdout_flush(void) { fflush(stdout); return 0; }

int64_t cato_stderr(const char *s) {
    fputs(s, stderr);
    fflush(stderr);
    return 0;
}
