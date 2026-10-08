/* C++ same-arity overloads (spec 024, FR-2), NEGATIVE — the distinct-member guard
   (`p6g_ctor_int_vs_ptr_distinct_negative`). The same two constructors as CPP_118, but the sink
   reads the member the SELECTED overload does not write: `Box b(&a)` runs `Box(Box*)`, which sets
   `v` to the constant 0 and only touches `w`. So `b.get()` (which reads `v`) is untainted at
   runtime, and resolving the construction by argument shape must not leak the `int` body's
   `v = x` into it.

   This is the precision guard on FR-2: it fails if the shape match is dropped and the
   construction falls back to every candidate of the arity, because the `int` candidate would
   then write `v` from `&a`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}
    Box(Box* o) : v(0), w(o->v) {}   /* v is the constant 0 on this path */
    int get() { return v; }
    int getw() { return w; }
};

int main() {
    Box a(source());
    Box b(&a);         /* Box::Box#1(Box*) only — b.v = 0, b.w = a.v */
    sink(b.get());     /* reads v, which the selected overload never taints */
    return 0;
}
