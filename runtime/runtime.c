#include <stdio.h>
#include <stdint.h>
void cato_print_f64(double v) { printf("%g\n", v); }
void cato_print_i64(int64_t v) { printf("%lld\n", (long long)v); }
void cato_print_string(const char *s) { printf("%s\n", s); }
