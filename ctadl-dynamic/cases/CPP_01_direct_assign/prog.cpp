/* C++ Milestone 1: taint flows directly from source()'s return into sink()'s
 * argument — the C++ counterpart of cases/01_direct_assign.
 *
 * source()/sink() are `extern "C"` prototypes so they bind to the unmangled marker
 * symbols the harness supplies: the inert static_markers_cpp.cpp bodies on CTADL's
 * static side, and the instrumented dfsan_shim_cpp.cpp bodies on the DFSan dynamic side. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    sink(s);
    return 0;
}
