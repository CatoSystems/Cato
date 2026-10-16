// alloc_profile.c — allocation *attribution* for the kernel process.
//
// Same interposition trick as alloc_count.c, but instead of a single total it
// attributes every allocation to its call site (the return address of the
// malloc/calloc/realloc call) and reports count + bytes per site.
//
// Build:  clang -dynamiclib -O2 -o /tmp/alloc_profile.dylib tools/alloc_profile.c
// Run:    DYLD_INSERT_LIBRARIES=/tmp/alloc_profile.dylib \
//           ALLOC_PROFILE=/tmp/prof.txt /tmp/k1 build selfhost/cato.cato /tmp/k2
// Output: /tmp/prof.txt, one line per site, sorted by bytes:
//           bytes<TAB>count<TAB>symbol
//
// Notes:
//  - Symbols come from dladdr, resolved once per unique address, so the
//    binary needs a (non-stripped) symbol table. Clang-built kernels have one.
//  - A recursion guard keeps dladdr's own allocations (Mach-O parsing) from
//    re-entering the hook.
#include <dlfcn.h>
#include <malloc/malloc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define NBUCKET (1u << 18)
#define GUARD 4096

struct ent {
  void *addr;
  const char *sym;
  uint64_t bytes;
  uint64_t calls;
};

static struct ent tab[NBUCKET];
static uint64_t total = 0, calls = 0;
static uint64_t other_bytes = 0, other_calls = 0;
static __thread int in_hook = 0;

static inline uint32_t hash_ptr(void *p) {
  uint64_t x = (uint64_t)(uintptr_t)p;
  x ^= x >> 33;
  x *= 0xff51afd7ed558ccdULL;
  x ^= x >> 33;
  return (uint32_t)(x & (NBUCKET - 1));
}

static const char *sym_of(void *addr) {
  Dl_info info;
  in_hook++;
  if (dladdr(addr, &info) && info.dli_sname) {
    in_hook--;
    return info.dli_sname;
  }
  in_hook--;
  return "??";
}

static void record(void *addr, size_t sz) {
  total += sz;
  calls++;
  uint32_t i = hash_ptr(addr);
  for (uint32_t n = 0; n < 64; n++) {
    uint32_t k = (i + n) & (NBUCKET - 1);
    if (tab[k].addr == addr) {
      tab[k].bytes += sz;
      tab[k].calls++;
      return;
    }
    if (tab[k].addr == NULL) {
      tab[k].addr = addr;
      tab[k].sym = sym_of(addr);
      tab[k].bytes = sz;
      tab[k].calls = 1;
      return;
    }
  }
  other_bytes += sz;
  other_calls++;
}

static void *my_malloc(size_t sz) {
  if (in_hook) return malloc_zone_malloc(malloc_default_zone(), sz);
  in_hook++;
  record(__builtin_return_address(0), sz);
  in_hook--;
  return malloc_zone_malloc(malloc_default_zone(), sz);
}

static void *my_calloc(size_t n, size_t s) {
  if (in_hook) return malloc_zone_calloc(malloc_default_zone(), n, s);
  in_hook++;
  record(__builtin_return_address(0), n * s);
  in_hook--;
  return malloc_zone_calloc(malloc_default_zone(), n, s);
}

static void *my_realloc(void *p, size_t sz) {
  if (in_hook) return malloc_zone_realloc(malloc_default_zone(), p, sz);
  in_hook++;
  record(__builtin_return_address(0), sz);
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

static int cmp_ent(const void *a, const void *b) {
  const struct ent *x = a, *y = b;
  if (x->bytes != y->bytes) return x->bytes < y->bytes ? 1 : -1;
  return 0;
}

__attribute__((destructor)) static void dump(void) {
  in_hook = 1; // our own stdio/allocations must not pollute the totals
  const char *path = getenv("ALLOC_PROFILE");
  FILE *f = path ? fopen(path, "w") : stderr;
  if (!f) f = stderr;

  struct ent *live = malloc(sizeof(struct ent) * NBUCKET);
  size_t n = 0;
  if (live) {
    for (uint32_t i = 0; i < NBUCKET; i++)
      if (tab[i].addr != NULL) live[n++] = tab[i];
    qsort(live, n, sizeof(struct ent), cmp_ent);
  }

  fprintf(f, "# ALLOC_BYTES %llu ALLOC_CALLS %llu OTHER_BYTES %llu OTHER_CALLS %llu\n",
          (unsigned long long)total, (unsigned long long)calls,
          (unsigned long long)other_bytes, (unsigned long long)other_calls);
  for (size_t i = 0; i < n; i++)
    fprintf(f, "%llu\t%llu\t%s\n", (unsigned long long)live[i].bytes,
            (unsigned long long)live[i].calls, live[i].sym);
  if (f != stderr) fclose(f);
  free(live);
}
