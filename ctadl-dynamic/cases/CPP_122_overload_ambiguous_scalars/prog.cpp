/* C++ same-arity overloads (spec 024, FR-3), EXPECTED PRECISION GAP — ambiguity is a
   multi-target edge, not a guess (`p6e_ctor_int_vs_long_ambiguous_negative`). `Box(int)` and
   `Box(long)` differ only in a SCALAR parameter type, and this slice deliberately does not type
   scalar expressions: `source()`'s return is `Other`, and both candidates' parameters are
   `Other` too, so the frontend cannot tell which constructor `Box b(source())` runs. It emits
   ONE construction whose targets are both keys — the same multi-target `DirectCall` a virtual
   call emits — which is a sound superset of the one C++ selects.

   C++ picks `Box(int)` here, so `w` stays 0 and DFSan sees no flow; CTADL reports the `long`
   body's `w = (int)x` as well. That is the declared price of soundness over the old chimera's
   luck (which agreed only because the carrying body happened to be defined first), and the
   positive half of the same shape -- `sink(b.get())`, which the int body taints -- must keep
   agreeing. s=flow d=none precision-disagree, labelled "overload-ambiguous-scalars". */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}         /* the overload C++ actually selects */
    Box(long x) : v(0), w((int)x) {}   /* also a target of the one call edge */
    int get() { return v; }
    int getw() { return w; }
};

int main() {
    Box b(source());   /* Box::Box#1(int) AND Box::Box#1(long) — one call, two edges */
    sink(b.getw());    /* only the long body writes w, and C++ never runs it */
    return 0;
}
