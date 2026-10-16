#include <stdlib.h>
#include <stdint.h>
#include <stdio.h>
#include <malloc/malloc.h>

static uint64_t total = 0;
static uint64_t calls = 0;

static void *my_malloc(size_t sz) { total += sz; calls++; return malloc_zone_malloc(malloc_default_zone(), sz); }
static void *my_calloc(size_t n, size_t s) { total += n * s; calls++; return malloc_zone_calloc(malloc_default_zone(), n, s); }
static void *my_realloc(void *p, size_t sz) { total += sz; calls++; return malloc_zone_realloc(malloc_default_zone(), p, sz); }
static void my_free(void *p) { malloc_zone_free(malloc_default_zone(), p); }

#define DYLD_INTERPOSE2(_replacement, _replacee) \
    __attribute__((used)) static struct { const void *replacement; const void *replacee; } \
    _interpose_##_replacee __attribute__((section("__DATA,__interpose"))) = \
    { (const void *)(unsigned long)&_replacement, (const void *)(unsigned long)&_replacee };

DYLD_INTERPOSE2(my_malloc, malloc)
DYLD_INTERPOSE2(my_calloc, calloc)
DYLD_INTERPOSE2(my_realloc, realloc)
DYLD_INTERPOSE2(my_free, free)

__attribute__((destructor)) static void dump(void) {
    uint64_t t = total, c = calls;
    fprintf(stderr, "ALLOC_BYTES %llu ALLOC_CALLS %llu\n",
            (unsigned long long)t, (unsigned long long)c);
}
