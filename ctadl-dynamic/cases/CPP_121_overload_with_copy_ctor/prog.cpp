/* C++ same-arity overloads (spec 024, FR-2), POSITIVE — the copy constructor keeps its
   precedence (`p6h_ctor_int_ptr_and_copy_coexist`). Three one-argument constructors coexist:
   `Box(int)`, `Box(Box*)` and the copy constructor `Box(const Box&)`. The copy constructor has a
   key no arity or signature can name (`Box::Box#copy`, spec 022), so it is not a member of the
   arity group at all; the other two are, and get `Box::Box#1(int)` / `Box::Box#1(Box*)`.

   So `Box b(a)` must still reach `Box::Box#copy` (the copy key wins over any same-arity
   sibling), while `Box c(&a)` reaches `Box::Box#1(Box*)` by argument shape. Both write `w` from
   `a.v`, so the sum flows. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}
    Box(Box* o) : v(0), w(o->v) {}
    Box(const Box& o) : v(0), w(o.v) {}   /* keyed Box::Box#copy, outside the arity group */
    int get() { return v; }
    int getw() { return w; }
};

int main() {
    Box a(source());        /* Box::Box#1(int)  — a.v is tainted */
    Box b(a);               /* Box::Box#copy    — b.w = a.v */
    Box c(&a);              /* Box::Box#1(Box*) — c.w = a.v */
    sink(b.getw() + c.getw());
    return 0;
}
