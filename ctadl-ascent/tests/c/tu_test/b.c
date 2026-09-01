/* b.c -- the CALLEE side (and, for case 5, a caller into a.c). Same convention: a
 * complete translation unit with its prototypes inlined. */

int source(void);
void sink_forward(int v);
void sink_global(int v);
void sink_reverse(int v);

int k(int x);            /* defined in a.c: passes x to sink_reverse */

extern int shared;       /* defined in a.c */

/* 1. forward, callee half. */
int g(int x) {
    sink_forward(x);
    return 0;
}

/* 2. and 3., callee half. */
int h(int x) {
    return x;
}

/* 4. global, reader half. */
void case_global_get(void) {
    sink_global(shared);
}

/* 5. reverse: this TU is the caller, a.c the callee. */
void case_reverse(void) {
    int t = source();
    k(t);
}
