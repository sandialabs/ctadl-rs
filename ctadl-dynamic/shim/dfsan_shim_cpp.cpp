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
#include <stdlib.h>

/* Single taint label for v1. Must match the runner's label->bit mapping
 * (label "Test" -> bit value 1). */
#define LABEL_TEST 1

/*
 * Allocation operators for `new`/`delete` programs (spec 014). Under `-nostdlib++`
 * the C++ runtime's `operator new`/`operator delete` are absent, so a `new Box()` /
 * `delete p` program fails to link (`undefined reference to 'operator new(unsigned
 * long) [clone .dfsan]'`). Defining them here — backed by malloc/free and compiled
 * with DFSan alongside each case, so the `.dfsan` clones exist — fixes the link. This
 * is taint-neutral (malloc/free move no labels) and regresses nothing: no existing
 * case uses `new`, and a program without `new`/`delete` never references these.
 */
void* operator new(unsigned long n) { return malloc(n); }
void operator delete(void* p) noexcept { free(p); }
void operator delete(void* p, unsigned long) noexcept { free(p); }

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
