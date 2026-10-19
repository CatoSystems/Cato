// alloc_live.c — live-heap accounting for one process (macOS).
//
// alloc_count.c measures churn (bytes requested over the whole run). Churn
// says nothing about retention: a run can churn 4 GB and peak at 10 MB, or
// churn 300 MB and keep all of it. This interposer tracks the *live* heap:
// every malloc/calloc/realloc adds malloc_size(p) (the allocator's real
// block size, quantum rounding included), every free subtracts it.
//
// Build:  clang -dynamiclib -O2 -o /tmp/alloc_live.dylib tools/alloc_live.c
// Run:    DYLD_INSERT_LIBRARIES=/tmp/alloc_live.dylib /tmp/k1 compile x.cato -o x
// Report (stderr, at exit):
//   LIVE_PEAK <bytes>        high-water mark of live heap (block sizes)
//   LIVE_EXIT <bytes>        still live at exit (never freed)
//   REQ_BYTES <bytes>        churn, requested sizes (== alloc_count's metric)
//   BLOCK_BYTES <bytes>      churn, block sizes (REQ vs BLOCK = rounding waste)
//   ALLOCS <n> FREES <n>     call counts
//
// Hardened children (clang) strip DYLD_INSERT_LIBRARIES, so only the
// compiler process itself is measured. Counters are atomic: the seed is a
// Rust binary and may allocate from more than one thread.
#include <malloc/malloc.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static _Atomic int64_t live = 0;
static _Atomic int64_t peak = 0;
static _Atomic uint64_t req = 0, block = 0, allocs = 0, frees = 0;

static void add(void *p, size_t asked) {
  if (!p) return;
  int64_t sz = (int64_t)malloc_size(p);
  atomic_fetch_add(&req, asked);
  atomic_fetch_add(&block, (uint64_t)sz);
  atomic_fetch_add(&allocs, 1);
  int64_t now = atomic_fetch_add(&live, sz) + sz;
  int64_t pk = atomic_load(&peak);
  while (now > pk && !atomic_compare_exchange_weak(&peak, &pk, now)) {}
}

static void sub(void *p) {
  if (!p) return;
  atomic_fetch_sub(&live, (int64_t)malloc_size(p));
  atomic_fetch_add(&frees, 1);
}

static void *my_malloc(size_t sz) {
  void *p = malloc_zone_malloc(malloc_default_zone(), sz);
  add(p, sz);
  return p;
}
static void *my_calloc(size_t n, size_t s) {
  void *p = malloc_zone_calloc(malloc_default_zone(), n, s);
  add(p, n * s);
  return p;
}
static void *my_realloc(void *old, size_t sz) {
  sub(old);
  void *p = malloc_zone_realloc(malloc_default_zone(), old, sz);
  add(p, sz);
  return p;
}
static void my_free(void *p) {
  sub(p);
  malloc_zone_free(malloc_default_zone(), p);
}

#define DYLD_INTERPOSE2(_replacement, _replacee)                               \
  __attribute__((used)) static struct {                                        \
    const void *replacement;                                                   \
    const void *replacee;                                                      \
  } _interpose_##_replacee __attribute__((section("__DATA,__interpose"))) = {  \
      (const void *)(unsigned long)&_replacement,                              \
      (const void *)(unsigned long)&_replacee};

DYLD_INTERPOSE2(my_malloc, malloc)
DYLD_INTERPOSE2(my_calloc, calloc)
DYLD_INTERPOSE2(my_realloc, realloc)
DYLD_INTERPOSE2(my_free, free)

__attribute__((destructor)) static void dump(void) {
  fprintf(stderr,
          "LIVE_PEAK %lld\nLIVE_EXIT %lld\nREQ_BYTES %llu\nBLOCK_BYTES %llu\n"
          "ALLOCS %llu FREES %llu\n",
          (long long)peak, (long long)live, (unsigned long long)req,
          (unsigned long long)block, (unsigned long long)allocs,
          (unsigned long long)frees);
}
