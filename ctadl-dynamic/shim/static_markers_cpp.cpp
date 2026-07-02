/*
 * static_markers_cpp.cpp — inert source()/sink() bodies for the C++ STATIC (CTADL) run.
 *
 * The C++ counterpart of shim/static_markers.c. CTADL's model only matches *defined*
 * functions, so source()/sink() must have bodies for the markers.json model to tag them
 * as taint endpoints. These bodies are deliberately inert: the model (not the body)
 * decides taint, so what they do at runtime is irrelevant here. The DFSan run uses
 * dfsan_shim_cpp.cpp instead.
 *
 * `extern "C"` so the identifiers CTADL sees are unmangled `source` / `sink`, matching the
 * `.*source.*` / `.*sink.*` model patterns. Each prog.cpp declares the prototypes; the
 * runner concatenates this file so CTADL sees real definitions.
 */
extern "C" int source() { return 0; }
extern "C" void sink(int x) { return; }

/*
 * Allocation operators for the well-formedness compile of `new`/`delete` programs (spec 014).
 * `clang_compiles` links prog.cpp + this file with a plain `clang++ -nostdlib++`; under
 * `-nostdlib++` the C++ runtime's `operator new`/`operator delete` are absent, so a `new
 * Box()`/`delete p` program would not link (its `compiles` well-formedness flag would read
 * "NO"). Defining them here — backed by libc's malloc/free (forward-declared, not via a header,
 * so the concatenated static-analysis source stays a clean parse) — lets those programs link.
 * The bodies are never analyzed: `operator new`/`operator delete` are not plain-identifier
 * function names, so CTADL's frontend does not lower them (they are inert here, like the
 * source/sink bodies above). A program without `new`/`delete` never references them.
 */
extern "C" void *malloc(unsigned long);
extern "C" void free(void *);
void *operator new(unsigned long n) { return malloc(n); }
void operator delete(void *p) noexcept { free(p); }
void operator delete(void *p, unsigned long) noexcept { free(p); }
