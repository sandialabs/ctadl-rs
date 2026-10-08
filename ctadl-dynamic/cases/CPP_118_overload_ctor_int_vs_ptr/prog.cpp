/* C++ same-arity overloads (spec 024, FR-2), POSITIVE — the constructor shape
   (`p6a_ctor_int_vs_ptr_ptr_body`). `Box(int)` and `Box(Box*)` share a name AND an arity, so
   before this slice they shared one key (`Box::Box`) and one FunctionIdx: the second body's
   parameter list was appended to the first's, giving the chimera `(this, x, this, o)`, and a
   one-argument construction bound only `x`. `Box b(&a)` therefore ran the `int` body, `o` was a
   parameter nothing ever passed, and the taint `w = o->v` carries was dropped (s=none against
   DFSan's d=flow).

   Each definition now has a key of its own — `Box::Box#1(int)` and `Box::Box#1(Box*)` — and the
   construction resolves by argument SHAPE: `&a` is a pointer to a known `Box` object, which only
   the `Box*` parameter accepts, so exactly one edge is emitted and it reaches the body that
   reads `o->v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}       /* the int overload: writes v */
    Box(Box* o) : v(0), w(o->v) {}   /* the pointer overload: copies the pointee's v into w */
    int get() { return v; }
    int getw() { return w; }
};

int main() {
    Box a(source());     /* Box::Box#1(int)  — a.v is tainted */
    Box b(&a);           /* Box::Box#1(Box*) — b.w = a.v */
    sink(b.getw());
    return 0;
}
