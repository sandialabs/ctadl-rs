/* a.c -- the CALLER side of every cross-TU case, written as its preprocessed form: the
 * prototypes a header would supply are inlined, so this file is a complete translation
 * unit on its own. Nothing here is defined in b.c except through those prototypes. */

int source(void);
void sink_intra(int v);
void sink_forward(int v);
void sink_return(int v);
void sink_fp(int v);
void sink_global(int v);
void sink_reverse(int v);

int g(int x);            /* defined in b.c: passes x to sink_forward */
int h(int x);            /* defined in b.c: returns x */

int shared;              /* defined here, read in b.c */

/* 0. baseline: the whole flow is inside this TU. Must be found in BOTH modes; if it is
 *    not, the harness is broken, not the linking. */
static int id_local(int x) { return x; }
void case_intra(void) {
    int t = source();
    sink_intra(id_local(t));
}

/* 1. forward: the tainted argument crosses into g's body (b.c), where the sink is. */
void case_forward(void) {
    int t = source();
    g(t);
}

/* 2. return: h (b.c) returns its argument; the sink is back here. */
void case_return(void) {
    int t = source();
    sink_return(h(t));
}

/* 3. function pointer: the pointer is bound to h, a function this TU never defines. */
void case_fp(void) {
    int (*fp)(int) = h;
    int t = source();
    sink_fp(fp(t));
}

/* 4. global: the taint crosses TUs through a file-scope object, not a call. The driver
 *    calls the writer here and the reader in b.c, so there is one call chain for the
 *    analysis to follow -- the cross-TU step is the store to `shared`, not a call edge. */
void case_global_get(void);
void case_global_set(void) {
    shared = source();
}
void case_global(void) {
    case_global_set();
    case_global_get();
}

/* 5. reverse: the callee side. b.c's case_reverse calls k, defined here. */
int k(int x) {
    sink_reverse(x);
    return 0;
}
