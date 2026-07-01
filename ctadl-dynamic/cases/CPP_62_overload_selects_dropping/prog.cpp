/* C++ overloading (by arity), free functions — NEGATIVE: precise selection, no cross-resolution.
   `g` is overloaded: `g(int)` returns its argument (WOULD flow), `g(int,int)` DROPS its args
   (returns a constant 0). The call `g(source(), 0)` has TWO explicit arguments, so it must
   resolve to the arity-2 overload (`g#2`), which discards its arguments — so NO taint reaches the
   sink. It must NOT cross-resolve to the flowing arity-1 sibling `g#1`. This is the discriminator
   that fails if overloads merge: a merged `g` (or a wrong pick of `g#1`) would leak `source` and
   report `s=flow d=none` (a soundness-side false positive). DFSan runs exactly the arity-2 `g`
   (drops), so it sees no flow; CTADL must match precisely (`s=none d=none`). */
extern "C" int source();
extern "C" void sink(int);

int g(int a) { return a; }
int g(int a, int b) { return 0; }

int main() {
    sink(g(source(), 0));
    return 0;
}
