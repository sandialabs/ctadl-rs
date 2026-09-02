/* C++ copy constructors (spec 022, FR-3), POSITIVE — the copy a BY-VALUE PARAMETER makes.
   `take(Box b)` takes its parameter by value, so C++ copy-constructs `b` from the argument at the
   call: `take(a)` runs `Box(const Box&)`, whose `w(o.v)` moves `a`'s tainted `v` into the
   parameter's `w`. Spec 020 made the parameter a receiver (`b.getw()` dispatches), but nothing ran
   the copy constructor at the call, so `b.w` came from the caller's clean `a.w` and CTADL reported
   s=none against DFSan's d=flow. Spec 022 records, per function, which explicit parameters are
   by-value class objects, and at every call site copy-constructs such an argument into a fresh
   object — `Box::Box#copy(&<t>, a)` — and passes that object instead. A BY-REFERENCE parameter
   (`take(Box&)`) records nothing and still copies nothing. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}
    Box(const Box& o) : v(0), w(o.v) {}   /* runs at the call, on the parameter's object */
    int getw() { return w; }
};

void take(Box b) { sink(b.getw()); }      /* by value: `b` IS the copy the constructor built */

int main() {
    Box a(source());                      /* a.v tainted, a.w clean */
    take(a);
    return 0;
}
