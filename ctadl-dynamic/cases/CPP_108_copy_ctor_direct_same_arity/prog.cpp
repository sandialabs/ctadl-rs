/* C++ copy constructors (spec 022, FR-1/FR-2), POSITIVE — the probed soundness gap
   (`p1a_copy_ctor_direct`). `Box` declares TWO one-argument constructors, `Box(int)` and the
   copy constructor `Box(const Box&)`. Overload resolution in this frontend is by ARITY, so both
   keyed the single name `Box::Box`, were counted as one arity (never mangled), and had both
   bodies lowered into one FunctionIdx — the second appending its parameters to the first's. Every
   one-argument construction then resolved to that chimera and the copy `Box b(a)` lost the flow
   (s=none against DFSan's d=flow). Spec 022 registers and lowers the copy constructor under a key
   of its own, `Box::Box#copy`, which is neither counted in `Box::Box`'s arity set nor mangled by
   it, and lowers `Box b(a)` — a construction whose single argument names a same-class object — to
   `DirectCall Box::Box#copy(&b, a)`. The copy constructor's `v(o.v)` then carries `a`'s tainted
   member into `b`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) : v(x) {}               /* arity 1 */
    Box(const Box& o) : v(o.v) {}      /* arity 1 as well — the collision this case pins */
    int get() { return v; }
};

int main() {
    Box a(source());                   /* Box::Box   — the int constructor */
    Box b(a);                          /* Box::Box#copy — the copy constructor, run on the copy */
    sink(b.get());
    return 0;
}
