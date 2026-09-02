/* C++ copy constructors (spec 022, FR-2), NEGATIVE — the field-sensitivity control
   (`p1e_copy_ctor_distinct`). The copy constructor here copies member-wise (`v(o.v), w(o.w)`),
   `a.v` is tainted and `a.w` is 0, and the sink reads the copy's DISTINCT member `w`. Running the
   copy constructor keeps the two members apart exactly as the value copy did, so no taint crosses
   from `v` to `w`. This is the case that goes s=flow the moment the copy edge conflates the object
   (a whole-object argument alias, or a copy constructor merged with its same-arity sibling
   `Box(int)`, which writes `v` from its one argument). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box(int x) : v(x), w(0) {}
    Box(const Box& o) : v(o.v), w(o.w) {}   /* member-wise: w comes from w, never from v */
    int getw() { return w; }
};

int main() {
    Box a(source());                        /* a.v tainted, a.w clean */
    Box b(a);                               /* Box::Box#copy: b.w = a.w = 0 */
    sink(b.getw());
    return 0;
}
