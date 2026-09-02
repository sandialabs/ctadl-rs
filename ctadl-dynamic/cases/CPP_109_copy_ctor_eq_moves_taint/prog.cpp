/* C++ copy constructors (spec 022, FR-2), POSITIVE — the probed soundness gap
   (`p1i_copy_ctor_eq_moves_taint`). The copy constructor MOVES taint between members: it writes
   `v(0), w(o.v)`, so a copy's `w` holds the source's `v`. `Box b = a` was lowered as a whole-object
   VALUE copy, which is exactly the *implicit* member-wise copy constructor (and is exact for it,
   see CPP_95) but is not what a user-defined one does — under the value copy `b.w` came from `a.w`,
   which is 0, so CTADL reported s=none against DFSan's d=flow. Spec 022 recognizes a copy
   initialization from a same-class object and lowers it to `DirectCall Box::Box#copy(&b, a)`, so
   the constructor's member-initializer list runs and `b.w` gets `a.v`. The distinct-member half is
   pinned by CPP_111. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}
    Box(const Box& o) : v(0), w(o.v) {}   /* the copy MOVES v into w */
    int getw() { return w; }
};

int main() {
    Box a(source());                      /* a.v tainted, a.w clean */
    Box b = a;                            /* runs Box::Box#copy: b.v = 0, b.w = a.v */
    sink(b.getw());
    return 0;
}
