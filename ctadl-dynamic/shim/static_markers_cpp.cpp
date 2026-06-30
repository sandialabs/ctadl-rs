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
