// dbg_hit.c — inclusive allocation attribution per clone call site.
//
// Built as a *dylib* and injected with DYLD_INSERT_LIBRARIES, because
// interposition only works from a library, not from the main executable.
// Paired with tools/instr_clones.py, which brackets every `clone__X(...)`
// call with __dbg_enter/__dbg_exit and counts `drop__X(...)` calls.
// Interposing malloc here attributes each allocation to the *outermost*
// bracketed clone call enclosing it, so one row answers "how much churn
// does this call site start". Unbracketed allocations (the kernel's own
// vector/cat buffers) land in the "outside" bucket.
//
// Build:  clang -dynamiclib -O2 -o /tmp/dbg_hit.dylib tools/dbg_hit.c
// Link:   clang -O2 -Wl,-undefined,dynamic_lookup /tmp/instr.ll \
//           runtime/runtime.c -o /tmp/ki
// Run:    DYLD_INSERT_LIBRARIES=/tmp/dbg_hit.dylib DBG_MAP=/tmp/map.tsv \
//           /tmp/ki build selfhost/cato/cli.cato /tmp/k3 2>/tmp/sites.txt
#include <malloc/malloc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define NID 65536

static uint64_t hits[NID];    // drop__ call counts
static uint64_t calls[NID];   // clone__ call counts (outermost only)
static uint64_t bytes[NID];   // bytes allocated inside (outermost only)
static int depth = 0;
static int32_t current = -1;
static __thread int in_hook = 0;
static uint64_t outside_bytes = 0, total_bytes = 0, total_calls = 0;

void __dbg_enter(int32_t id) {
  if (depth++ == 0) current = id;
}

void __dbg_exit(int32_t id) {
  (void)id;
  if (--depth == 0) current = -1;
}

void __dbg_hit(int32_t id) {
  if (id >= 0 && id < NID) hits[id]++;
}

static inline void account(size_t sz) {
  total_bytes += sz;
  total_calls++;
  if (current >= 0) {
    bytes[current] += sz;
    calls[current]++;
  } else {
    outside_bytes += sz;
  }
}

static void *my_malloc(size_t sz) {
  if (in_hook) return malloc_zone_malloc(malloc_default_zone(), sz);
  in_hook++;
  account(sz);
  in_hook--;
  return malloc_zone_malloc(malloc_default_zone(), sz);
}
static void *my_calloc(size_t n, size_t s) {
  if (in_hook) return malloc_zone_calloc(malloc_default_zone(), n, s);
  in_hook++;
  account(n * s);
  in_hook--;
  return malloc_zone_calloc(malloc_default_zone(), n, s);
}
static void *my_realloc(void *p, size_t sz) {
  if (in_hook) return malloc_zone_realloc(malloc_default_zone(), p, sz);
  in_hook++;
  account(sz);
  in_hook--;
  return malloc_zone_realloc(malloc_default_zone(), p, sz);
}
static void my_free(void *p) { malloc_zone_free(malloc_default_zone(), p); }

#define DYLD_INTERPOSE2(_replacement, _replacee)                             \
  __attribute__((used)) static struct {                                      \
    const void *replacement;                                                 \
    const void *replacee;                                                    \
  } _interpose_##_replacee __attribute__((section("__DATA,__interpose"))) = { \
      (const void *)(unsigned long)&_replacement,                            \
      (const void *)(unsigned long)&_replacee};

DYLD_INTERPOSE2(my_malloc, malloc)
DYLD_INTERPOSE2(my_calloc, calloc)
DYLD_INTERPOSE2(my_realloc, realloc)
DYLD_INTERPOSE2(my_free, free)

__attribute__((destructor)) static void dump(void) {
  in_hook = 1;
  const char *map = getenv("DBG_MAP");
  if (!map) return;
  FILE *f = fopen(map, "r");
  if (!f) return;
  static char fns[NID][48], cals[NID][48];
  char line[256];
  while (fgets(line, sizeof line, f)) {
    char *t1 = strchr(line, '\t');
    if (!t1) continue;
    char *t2 = strchr(t1 + 1, '\t');
    if (!t2) continue;
    *t1 = 0;
    *t2 = 0;
    int id = atoi(line);
    if (id < 0 || id >= NID) continue;
    snprintf(fns[id], sizeof fns[id], "%s", t1 + 1);
    char *nl = strchr(t2 + 1, '\n');
    if (nl) *nl = 0;
    snprintf(cals[id], sizeof cals[id], "%s", t2 + 1);
  }
  fclose(f);
  for (int id = 0; id < NID; id++)
    if (calls[id] || hits[id])
      fprintf(stderr, "SITE\t%d\t%llu\t%llu\t%s\t%s\n", id,
              (unsigned long long)calls[id], (unsigned long long)bytes[id],
              fns[id], cals[id]);
  fprintf(stderr, "TOTAL\t%llu\t%llu\toutside=%llu\n",
          (unsigned long long)total_calls, (unsigned long long)total_bytes,
          (unsigned long long)outside_bytes);
}
