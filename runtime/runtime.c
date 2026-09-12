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
