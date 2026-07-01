/* C++ constructor overloading (by arity) — NEGATIVE: precise selection, no cross-resolution.
   `Box` declares `Box(int x){v=x;}` (arity 1, WOULD flow its arg into `v`) and
   `Box(int x,int y){v=0;}` (arity 2, DROPS both args, writing the constant 0). The construction
   `Box b(source(), 0)` has TWO explicit arguments, so it must resolve to the arity-2 ctor
   `Box::Box#2`, which discards its arguments — so NO taint reaches `v`, and `b.get()` reads an
   untainted 0. It must NOT cross-resolve to the flowing arity-1 sibling `Box::Box#1`. This is
   the discriminator that fails if the constructors merge: a merged `Box::Box` (or a wrong pick
   of `#1`) would leak `source` into `v` and report `s=flow d=none` (a soundness-side false
   positive). The negative is non-vacuous: `source()` IS called and tainted — it simply never
   reaches `v` because the selected ctor drops it. clang++ runs exactly the arity-2 ctor
   (drops), so DFSan sees no flow; CTADL must match precisely (`s=none d=none`). */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) { v = x; }
    Box(int x, int y) { v = 0; }
    int get() { return v; }
};

int main() {
    Box b(source(), 0);
    sink(b.get());
    return 0;
}
