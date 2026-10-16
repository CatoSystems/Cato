// dbg_hit.c — counter table for tools/instr_clones.py.
//
// Build:  clang -O2 -c -o /tmp/dbg_hit.o tools/dbg_hit.c
// Link:   clang -O2 /tmp/instr.ll runtime/runtime.c /tmp/dbg_hit.o -o /tmp/k2i
// Run:    DBG_MAP=/tmp/map.tsv /tmp/k2i build selfhost/cato.cato /tmp/k3i
// Output (stderr, at exit): "ID<TAB>count" for every hit site, busiest first.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define NID 65536

static uint64_t hits[NID];

void __dbg_hit(int32_t id) {
  if (id >= 0 && id < NID) hits[id]++;
}

__attribute__((destructor)) static void dump(void) {
  const char *map = getenv("DBG_MAP");
  if (!map) return;
  FILE *m = fopen(map, "r");
  if (!m) return;
  FILE *out = stderr;
  // id -> (fn, callee, ir)
  static char fns[NID][64], cals[NID][64];
  int id;
  while (fscanf(m, "%d\t%63[^\t]\t%63[^\t]", &id, fns[id], cals[id]) == 3) {
    int ch;
    while ((ch = fgetc(m)) != '\n' && ch != EOF) {
    }
  }
  fclose(m);
  for (int i = 0; i < NID; i++)
    if (hits[i])
      fprintf(out, "HIT\t%d\t%llu\t%s\t%s\n", i, (unsigned long long)hits[i], fns[i], cals[i]);
}
