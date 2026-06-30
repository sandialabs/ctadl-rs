/* F2 (soundness): function pointer stored in an ARRAY ELEMENT and called through it.
   The F1 fix resolved scalar funcptr vars (05/07/08) and struct fields (09), but a
   funcptr held in an array element still drops taint. CTADL reports NO flow here
   (soundness gap); the SOUNDNESS-GAP verdict is EXPECTED and intentionally preserved
   -- see ctadl-dynamic/KNOWN_FINDINGS.md (F2). Do not "fix" it by changing the oracle.
   Found by the broadened generator (M7). */
extern "C" int source();
extern "C" void sink(int);

int id(int p) { return p; }

int main() {
    int (*fps[2])(int);
    fps[0] = id;
    fps[1] = id;
    int s = source();
    int r = fps[0](s);
    sink(r);
    return 0;
}
