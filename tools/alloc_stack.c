// alloc_stack.c — byte-weighted stack sampling for the kernel process.
//
// Same interposition as alloc_count.c. Every SAMPLE_BYTES of allocated bytes
// (cumulative), one allocation is sampled and its Cato-level call stack is
// recorded. Each sample therefore stands for SAMPLE_BYTES of churn, so the
// per-signature share of the total is what matters, not the raw counts.
//
// Build:  clang -dynamiclib -O2 -o /tmp/alloc_stack.dylib tools/alloc_stack.c
// Run:    DYLD_INSERT_LIBRARIES=/tmp/alloc_stack.dylib \
//           ALLOC_STACK=/tmp/stack.txt /tmp/k1 build selfhost/cato.cato /tmp/k2
// Output: /tmp/stack.txt — "samples<TAB>approx_bytes<TAB>site <- caller <- ..."
#include <dlfcn.h>
#include <execinfo.h>
#include <malloc/malloc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define SAMPLE_BYTES (8u * 1024u * 1024u)
#define NBUCKET (1u << 16)
#define FRAMES 4
#define MAXSYM 256

struct site {
  char key[MAXSYM];
  uint64_t samples;
  uint64_t weight;
};

static struct site tab[NBUCKET];
static uint64_t total = 0, calls = 0, since = 0;
static __thread int in_hook = 0;

static inline uint32_t hash_str(const char *s) {
  uint64_t h = 1469598103934665603ULL;
  while (*s) {
    h ^= (unsigned char)*s++;
    h *= 1099511628211ULL;
  }
  return (uint32_t)(h & (NBUCKET - 1));
}

static int skip_frame(const char *n) {
  if (!n) return 1;
  static const char *skip[] = {"sym_of", "record", "my_malloc", "my_calloc",
                               "my_realloc", "dump", NULL};
  for (int i = 0; skip[i]; i++)
    if (strcmp(n, skip[i]) == 0) return 1;
  return 0;
}

static void sample(void) {
  void *fr[32];
  int n = backtrace(fr, 32);
  char buf[MAXSYM];
  size_t off = 0;
  int used = 0;
  buf[0] = 0;
  for (int i = 0; i < n && used < FRAMES; i++) {
    Dl_info info;
    if (!dladdr(fr[i], &info) || !info.dli_sname) continue;
    if (skip_frame(info.dli_sname)) continue;
    // Skip the interposition shim frames in the dylib itself.
    if (info.dli_fname && strstr(info.dli_fname, "alloc_stack")) continue;
    int w = snprintf(buf + off, sizeof(buf) - off, "%s%s",
                     used ? " <- " : "", info.dli_sname);
    if (w <= 0) break;
    off += (size_t)w;
    used++;
    if (off >= sizeof(buf)) break;
  }
  if (used == 0) snprintf(buf, sizeof(buf), "??");

  uint32_t i = hash_str(buf);
  for (uint32_t k = 0; k < 256; k++) {
    uint32_t j = (i + k) & (NBUCKET - 1);
    if (tab[j].key[0] == 0) {
      snprintf(tab[j].key, MAXSYM, "%s", buf);
      tab[j].samples = 1;
      tab[j].weight = SAMPLE_BYTES;
      return;
    }
    if (strcmp(tab[j].key, buf) == 0) {
      tab[j].samples++;
      tab[j].weight += SAMPLE_BYTES;
      return;
    }
  }
}

static inline void account(size_t sz) {
  total += sz;
  calls++;
  since += sz;
  if (since >= SAMPLE_BYTES) {
    since = 0;
    sample();
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

static int cmp_site(const void *a, const void *b) {
  const struct site *x = a, *y = b;
  if (x->weight != y->weight) return x->weight < y->weight ? 1 : -1;
  return 0;
}

__attribute__((destructor)) static void dump(void) {
  in_hook = 1;
  const char *path = getenv("ALLOC_STACK");
  FILE *f = path ? fopen(path, "w") : stderr;
  if (!f) f = stderr;

  struct site *live = malloc(sizeof(struct site) * NBUCKET);
  size_t n = 0;
  if (live) {
    for (uint32_t i = 0; i < NBUCKET; i++)
      if (tab[i].key[0]) live[n++] = tab[i];
    qsort(live, n, sizeof(struct site), cmp_site);
  }
  fprintf(f, "# ALLOC_BYTES %llu ALLOC_CALLS %llu SAMPLE_BYTES %u\n",
          (unsigned long long)total, (unsigned long long)calls, SAMPLE_BYTES);
  for (size_t i = 0; i < n; i++)
    fprintf(f, "%llu\t%llu\t%s\n", (unsigned long long)live[i].samples,
            (unsigned long long)live[i].weight, live[i].key);
  if (f != stderr) fclose(f);
  free(live);
}
