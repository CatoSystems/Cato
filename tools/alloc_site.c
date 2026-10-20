// alloc_site.c — attribute the *live* heap to allocating functions (macOS).
//
// alloc_live.c says how much is live; this says who allocated it. Every
// allocation records ptr -> (block size, site), where a site is the first
// (allocator, requester) pair found by a frame-pointer walk (arm64 macOS keeps frame
// pointers). free/realloc drop the record. The report lists, by symbol of
// the selected frame, the bytes and blocks live at the moment of the report:
//   - at exit by default, and
//   - root leaks with retained sizes, when ALLOC_SITE_ROOTS=1 (see
//     roots_report: ranks where ownership was lost), and
//   - at the live-heap peak too, when ALLOC_SITE_PEAK=1 (a snapshot is taken
//     each time live grows past the previous peak by >= 8 MiB, so the "peak"
//     report is within 8 MiB of the true peak; this is slow — use for
//     attribution, never for timing).
//
// Build:  clang -dynamiclib -O2 -o /tmp/alloc_site.dylib tools/alloc_site.c
// Run:    DYLD_INSERT_LIBRARIES=/tmp/alloc_site.dylib ALLOC_SITE_OUT=/tmp/site \
//           /tmp/k1 compile x.cato --emit-ir -o x.ll
// Output: /tmp/site.exit (and /tmp/site.peak), lines sorted by bytes:
//           live_bytes<TAB>blocks<TAB>allocator<TAB>requester
// Column 3 is the first frame outside runtime helpers (cato_*, strdup):
// the generated function that allocated. Column 4 is the first frame above
// it that is not clone__/drop__ glue (nor column 3's own function): who
// asked for the copy. Generated IR carries no "frame-pointer" attribute, so
// build the profiled compiler with frame records, e.g.
//   sed -E 's/^(define .*\)) \{$/\1 #99 {/' k1.ll > k1fp.ll
//   echo 'attributes #99 = { "frame-pointer"="all" }' >> k1fp.ll
#include <dlfcn.h>
#include <malloc/malloc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

#define FRAMES 2
#define PTAB_BITS 25 /* 32M slots */
#define PTAB (1u << PTAB_BITS)
#define STAB_BITS 20
#define STAB (1u << STAB_BITS)

struct prec { uintptr_t p; uint32_t sz; uint32_t site; };
struct site { uintptr_t pc[FRAMES]; int64_t live; int64_t blocks; int64_t pk_live; int64_t pk_blocks; int64_t r_roots; int64_t r_bytes; int64_t r_blocks; };

static struct prec *ptab;
static struct site *stab;
static uint32_t nsites = 1; /* 0 = empty */
static int64_t live = 0, peak = 0, snap_at = 0;
static int inited = 0, busy = 0, want_peak = 0;

static void init(void) {
  if (inited) return;
  inited = 1;
  ptab = mmap(0, sizeof(struct prec) * (size_t)PTAB, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  stab = mmap(0, sizeof(struct site) * (size_t)STAB, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  const char *e = getenv("ALLOC_SITE_PEAK");
  want_peak = e && *e == '1';
}

static inline uint32_t hp(uintptr_t p) { return (uint32_t)((p >> 4) * 0x9E3779B97F4A7C15ull >> (64 - PTAB_BITS)); }

/* pc -> symbol cache (dladdr once per distinct return address) */
#define CBITS 18
static struct { uintptr_t pc; const char *sym; int cls; } pcache[1u << CBITS];
static const char *sym(uintptr_t pc);
static int is_helper(const char *s);
/* class: 0 = generated code, 1 = runtime/libc helper, 2 = clone/drop glue */
static int cls_of(uintptr_t pc, const char **name) {
  uint32_t i = (uint32_t)((pc * 0x9E3779B97F4A7C15ull) >> (64 - CBITS));
  while (pcache[i].pc && pcache[i].pc != pc) i = (i + 1) & ((1u << CBITS) - 1);
  if (!pcache[i].pc) {
    const char *s = sym(pc);
    pcache[i].pc = pc; pcache[i].sym = s;
    pcache[i].cls = is_helper(s) ? 1 : (!strncmp(s, "clone__", 7) || !strncmp(s, "drop__", 6)) ? 2 : 0;
  }
  *name = pcache[i].sym;
  return pcache[i].cls;
}

/* Site = (A, B): A is the first frame outside runtime helpers (who called
   malloc, e.g. clone__Ast); B is the first frame above A that is neither a
   helper nor clone/drop glue nor A's own function (who asked for the
   copy). Recursion in glue thus collapses onto its real requester. */
__attribute__((noinline)) static uint32_t site_of(void) {
  uintptr_t pc[FRAMES] = {0};
  uintptr_t *fp = __builtin_frame_address(0);
  /* frames: site_of -> add -> my_* -> caller; skip to my_*'s record */
  fp = (uintptr_t *)fp[0];
  fp = (uintptr_t *)fp[0];
  const char *an = 0;
  int stage = 0;
  for (int d = 0; d < 256 && fp && stage < 2; d++) {
    uintptr_t r = fp[1];
    const char *n;
    int c = cls_of(r, &n);
    if (stage == 0 && c != 1) { pc[0] = r; an = n; stage = 1; }
    else if (stage == 1 && c == 0 && n != an) { pc[1] = r; stage = 2; }
    uintptr_t *nx = (uintptr_t *)fp[0];
    if (nx <= fp) break;
    fp = nx;
  }
  uint64_t h = 1469598103934665603ull;
  for (int i = 0; i < FRAMES; i++) h = (h ^ pc[i]) * 1099511628211ull;
  uint32_t i = (uint32_t)(h >> (64 - STAB_BITS));
  for (;;) {
    struct site *s = &stab[i];
    if (s->pc[0] == 0 && s->pc[1] == 0) {
      if (nsites >= STAB - 1) return 0;
      memcpy(s->pc, pc, sizeof pc);
      if (s->pc[0] == 0) s->pc[0] = 1;
      nsites++;
      return i;
    }
    if (memcmp(s->pc, pc, sizeof pc) == 0) return i;
    i = (i + 1) & (STAB - 1);
  }
}

static void snapshot(void) {
  for (uint32_t i = 0; i < STAB; i++) { stab[i].pk_live = stab[i].live; stab[i].pk_blocks = stab[i].blocks; }
}

__attribute__((noinline)) static void add(void *p) {
  if (!p || busy) return;
  init();
  busy = 1;
  uint32_t sz = (uint32_t)malloc_size(p);
  uint32_t s = site_of();
  uint32_t i = hp((uintptr_t)p);
  while (ptab[i].p && ptab[i].p != (uintptr_t)p) i = (i + 1) & (PTAB - 1);
  ptab[i].p = (uintptr_t)p; ptab[i].sz = sz; ptab[i].site = s;
  stab[s].live += sz; stab[s].blocks++;
  live += sz;
  if (live > peak) {
    peak = live;
    if (want_peak && peak >= snap_at + (8 << 20)) { snapshot(); snap_at = peak; }
  }
  busy = 0;
}

static void sub(void *p) {
  if (!p || !inited || busy) return;
  uint32_t i = hp((uintptr_t)p);
  while (ptab[i].p) {
    if (ptab[i].p == (uintptr_t)p) {
      struct prec r = ptab[i];
      stab[r.site].live -= r.sz; stab[r.site].blocks--;
      live -= r.sz;
      /* backward-shift deletion keeps probe chains intact */
      uint32_t j = i;
      for (;;) {
        ptab[i].p = 0;
        for (;;) {
          j = (j + 1) & (PTAB - 1);
          if (!ptab[j].p) return;
          uint32_t k = hp(ptab[j].p);
          if (i <= j ? (i < k && k <= j) : (i < k || k <= j)) continue;
          break;
        }
        ptab[i] = ptab[j];
        i = j;
      }
    }
    i = (i + 1) & (PTAB - 1);
  }
}

__attribute__((noinline)) static void *my_malloc(size_t sz) { void *p = malloc_zone_malloc(malloc_default_zone(), sz); add(p); return p; }
__attribute__((noinline)) static void *my_calloc(size_t n, size_t s) { void *p = malloc_zone_calloc(malloc_default_zone(), n, s); add(p); return p; }
__attribute__((noinline)) static void *my_realloc(void *o, size_t sz) { sub(o); void *p = malloc_zone_realloc(malloc_default_zone(), o, sz); add(p); return p; }
__attribute__((noinline)) static void my_free(void *p) { sub(p); malloc_zone_free(malloc_default_zone(), p); }

#define DYLD_INTERPOSE2(_r, _e)                                                \
  __attribute__((used)) static struct { const void *r; const void *e; }       \
      _interpose_##_e __attribute__((section("__DATA,__interpose"))) = {       \
          (const void *)(unsigned long)&_r, (const void *)(unsigned long)&_e};
DYLD_INTERPOSE2(my_malloc, malloc)
DYLD_INTERPOSE2(my_calloc, calloc)
DYLD_INTERPOSE2(my_realloc, realloc)
DYLD_INTERPOSE2(my_free, free)

static const char *sym(uintptr_t pc) {
  Dl_info d;
  if (pc > 1 && dladdr((void *)(pc - 1), &d) && d.dli_sname) return d.dli_sname;
  return "?";
}
static int is_helper(const char *s) {
  return !strncmp(s, "cato_", 5) || !strncmp(s, "_cato_", 6) || !strcmp(s, "strdup") ||
         !strcmp(s, "strndup") || !strncmp(s, "_malloc", 7) || !strncmp(s, "my_", 3) || !strcmp(s, "?");
}

struct agg { const char *a, *b; int64_t bytes, blocks, roots; };
static int cmpagg(const void *x, const void *y) {
  int64_t a = ((const struct agg *)x)->bytes, b = ((const struct agg *)y)->bytes;
  return a < b ? 1 : a > b ? -1 : 0;
}

static void report(const char *path, int use_peak, int frame) {
  FILE *f = fopen(path, "w");
  if (!f) return;
  struct agg *ag = mmap(0, sizeof(struct agg) * 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  int nag = 0;
  int64_t tot = 0;
  for (uint32_t i = 0; i < STAB; i++) {
    struct site *s = &stab[i];
    int64_t b = use_peak ? s->pk_live : s->live, n = use_peak ? s->pk_blocks : s->blocks;
    if (b <= 0) continue;
    tot += b;
    (void)frame;
    const char *a = sym(s->pc[0]);
    const char *c = s->pc[1] ? sym(s->pc[1]) : "";
    int j;
    for (j = 0; j < nag; j++) if (ag[j].a == a && ag[j].b == c) break;
    if (j == nag) { if (nag == 65535) continue; ag[nag].a = a; ag[nag].b = c; ag[nag].bytes = 0; ag[nag].blocks = 0; nag++; }
    ag[j].bytes += b; ag[j].blocks += n;
  }
  qsort(ag, nag, sizeof *ag, cmpagg);
  fprintf(f, "# total live %lld bytes (%s), peak %lld\n", (long long)tot, use_peak ? "at peak snapshot" : "at exit", (long long)peak);
  for (int j = 0; j < nag; j++) fprintf(f, "%lld\t%lld\t%s\t%s\n", (long long)ag[j].bytes, (long long)ag[j].blocks, ag[j].a, ag[j].b);
  fclose(f);
}


/* Root-leak analysis (ALLOC_SITE_ROOTS=1, at exit). A live block that no
   other live block points to (exact start-address match on any aligned
   word) is a root: whoever held it dropped it without freeing. Each root
   is credited with every block it reaches that no earlier root reached —
   its retained graph — so the report ranks the sites where ownership was
   lost, not the sites that merely allocated a subtree's leaves. Stack and
   globals are not scanned: run it at exit, when they hold nothing. */
static inline int64_t plookup(uintptr_t w) {
  if (!w || (w & 7)) return -1;
  uint32_t i = hp(w);
  while (ptab[i].p) {
    if (ptab[i].p == w) return i;
    i = (i + 1) & (PTAB - 1);
  }
  return -1;
}
static void roots_report(const char *path) {
  uint8_t *mark = mmap(0, PTAB, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  /* 1: referenced by another live block */
  for (uint32_t i = 0; i < PTAB; i++) {
    if (!ptab[i].p) continue;
    uintptr_t *w = (uintptr_t *)ptab[i].p;
    for (uint32_t k = 0; k < ptab[i].sz / 8; k++) {
      int64_t j = plookup(w[k]);
      if (j >= 0 && (uint32_t)j != i) mark[j] |= 1;
    }
  }
  uint32_t *stk = mmap(0, sizeof(uint32_t) * (size_t)PTAB, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  /* 2: DFS from each root; bit 2 = visited */
  for (int pass = 0; pass < 2; pass++)
  for (uint32_t i = 0; i < PTAB; i++) {
    if (!ptab[i].p || (mark[i] & 2)) continue;
    /* pass 0: true roots; pass 1: whatever is left (pure cycles) */
    if (pass == 0 && (mark[i] & 1)) continue;
    struct site *rs = &stab[ptab[i].site];
    rs->r_roots++;
    size_t sp = 0;
    stk[sp++] = i; mark[i] |= 2;
    while (sp) {
      uint32_t b = stk[--sp];
      rs->r_bytes += ptab[b].sz; rs->r_blocks++;
      uintptr_t *w = (uintptr_t *)ptab[b].p;
      for (uint32_t k = 0; k < ptab[b].sz / 8; k++) {
        int64_t j = plookup(w[k]);
        if (j >= 0 && !(mark[j] & 2)) { mark[j] |= 2; stk[sp++] = (uint32_t)j; }
      }
    }
  }
  FILE *f = fopen(path, "w");
  if (!f) return;
  struct agg *ag = mmap(0, sizeof(struct agg) * 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  int nag = 0;
  int64_t tot = 0;
  for (uint32_t i = 0; i < STAB; i++) {
    struct site *s = &stab[i];
    if (s->r_roots <= 0) continue;
    tot += s->r_bytes;
    const char *a = sym(s->pc[0]);
    const char *c = s->pc[1] ? sym(s->pc[1]) : "";
    int j;
    for (j = 0; j < nag; j++) if (ag[j].a == a && ag[j].b == c) break;
    if (j == nag) { if (nag == 65535) continue; ag[nag].a = a; ag[nag].b = c; ag[nag].bytes = 0; ag[nag].blocks = 0; ag[nag].roots = 0; nag++; }
    ag[j].bytes += s->r_bytes; ag[j].blocks += s->r_blocks; ag[j].roots += s->r_roots;
  }
  qsort(ag, nag, sizeof *ag, cmpagg);
  fprintf(f, "# retained_bytes\tretained_blocks\troots\troot allocator\troot requester (total %lld)\n", (long long)tot);
  for (int j = 0; j < nag; j++)
    fprintf(f, "%lld\t%lld\t%lld\t%s\t%s\n", (long long)ag[j].bytes, (long long)ag[j].blocks, (long long)ag[j].roots, ag[j].a, ag[j].b);
  fclose(f);
}

__attribute__((destructor)) static void dump(void) {
  if (!inited) return;
  busy = 1;
  const char *o = getenv("ALLOC_SITE_OUT");
  const char *fr = getenv("ALLOC_SITE_FRAME");
  int frame = fr ? atoi(fr) : 0;
  char path[1024];
  snprintf(path, sizeof path, "%s.exit", o ? o : "/tmp/alloc_site");
  report(path, 0, frame);
  if (want_peak) { snprintf(path, sizeof path, "%s.peak", o ? o : "/tmp/alloc_site"); report(path, 1, frame); }
  const char *r = getenv("ALLOC_SITE_ROOTS");
  if (r && *r == '1') { snprintf(path, sizeof path, "%s.roots", o ? o : "/tmp/alloc_site"); roots_report(path); }
}
