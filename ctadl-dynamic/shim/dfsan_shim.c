/*
 * dfsan_shim.c — instrumented source()/sink() for the DYNAMIC (DFSan) run.
 *
 * Compiled together with each case's prog.c under `clang -fsanitize=dataflow`.
 * source() taints its return value with the `Test` label; sink() reports whether
 * its argument carries that label, printing one machine-parseable observation
 * line per call which the dynamic runner parses.
 *
 * This is the runtime counterpart of shim/static_markers.c (which gives CTADL
 * inert bodies for the static run). Both match the `int source(); void sink(int);`
 * prototypes each prog.c declares.
 */
#include <sanitizer/dfsan_interface.h>
#include <stdio.h>

/* Single taint label for v1. Must match the runner's label->bit mapping
 * (label "Test" -> bit value 1). */
#define LABEL_TEST 1

int source() {
    int v = 0x7a; /* value is irrelevant; the shadow (label) is what propagates */
    dfsan_set_label(LABEL_TEST, &v, sizeof(v));
    return v;
}

void sink(int x) {
    dfsan_label l = dfsan_get_label(x);
    int test = dfsan_has_label(l, LABEL_TEST) ? 1 : 0;
    /* Observation line consumed by the dynamic runner. */
    printf("CTADL_DYN_OBSERVE sink test=%d label=%u\n", test, (unsigned)l);
    fflush(stdout);
}
