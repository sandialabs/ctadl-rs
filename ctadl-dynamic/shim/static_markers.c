/*
 * static_markers.c — inert source()/sink() bodies for the STATIC (CTADL) run.
 *
 * CTADL's model only matches *defined* functions, so source()/sink() must have
 * bodies for the markers.json model to tag them as taint endpoints. These
 * bodies are deliberately inert: the model (not the body) decides taint, so what
 * they do at runtime is irrelevant here. The DFSan run uses dfsan_shim.c instead.
 *
 * Each case's prog.c declares `int source();` / `void sink(int);` as prototypes;
 * the runner concatenates this file so CTADL sees real definitions.
 */
int source() { return 0; }
void sink(int x) { return; }
