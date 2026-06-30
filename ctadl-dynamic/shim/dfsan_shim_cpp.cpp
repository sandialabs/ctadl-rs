/*
 * dfsan_shim_cpp.cpp — instrumented source()/sink() for the C++ DYNAMIC (DFSan) run.
 *
 * The C++ counterpart of shim/dfsan_shim.c. Compiled together with each C++ case's
 * prog.cpp under `clang++ -fsanitize=dataflow -nostdlib++`. source() taints its return
 * value with the `Test` label; sink() reports whether its argument carries that label,
 * printing the same machine-parseable observation line the dynamic runner already parses.
 *
 * source/sink are `extern "C"` so name mangling doesn't break CTADL's `.*source.*` /
 * `.*sink.*` model matching or the shim's symbol names (matching the prototypes each
 * prog.cpp declares: `int source(); void sink(int);`).
 *
 * Uses the C header <stdio.h> (not <cstdio>): STL headers don't build under
 * -fsanitize=dataflow on this box, so the taint path is deliberately STL-free.
 */
#include <sanitizer/dfsan_interface.h>
#include <stdio.h>

/* Single taint label for v1. Must match the runner's label->bit mapping
 * (label "Test" -> bit value 1). */
#define LABEL_TEST 1

extern "C" int source() {
    int v = 0x7a; /* value is irrelevant; the shadow (label) is what propagates */
    dfsan_set_label(LABEL_TEST, &v, sizeof(v));
    return v;
}

extern "C" void sink(int x) {
    dfsan_label l = dfsan_get_label(x);
    int test = dfsan_has_label(l, LABEL_TEST) ? 1 : 0;
    /* Observation line consumed by the dynamic runner. */
    printf("CTADL_DYN_OBSERVE sink test=%d label=%u\n", test, (unsigned)l);
    fflush(stdout);
}
