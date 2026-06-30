/* Taint flows through an INDIRECT call: fp points at id, and the tainted value
   is passed through fp(s). Exercises indirect-call resolution + summary.

   KNOWN FINDING F1: CTADL reports NO flow here (soundness gap). The runner's
   SOUNDNESS-GAP verdict for this case is EXPECTED and intentionally preserved —
   see ctadl-dynamic/KNOWN_FINDINGS.md. Do not "fix" it by changing the oracle. */
extern "C" int source();
extern "C" void sink(int);

int id(int p) {
    return p;
}

int main() {
    int (*fp)(int) = id;
    int s = source();
    int r = fp(s);
    sink(r);
    return 0;
}
