/* C++ same-arity overloads (spec 024, FR-2), POSITIVE — the instance-method shape
   (`p6c_method_int_vs_ref`). `set(int)` and `set(Box&)` are one name at one arity, so they used
   to be lowered into one `Box::set` whose parameter list was `(this, x, this, o)`; `b.set(a)`
   bound `x` and left `o` unpassed, dropping the `w = o.v` flow (s=none against DFSan's d=flow).

   The two are now `Box::set#1(int)` and `Box::set#1(Box&)`, and the dispatch picks by argument
   shape: `a` names a known `Box` OBJECT, which a `Box&` parameter accepts and an `int` one does
   not. A reference parameter and a by-value one are the same shape (they differ in mode, not
   shape) and distinct keys, so `const Box&` would resolve here too. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box() : v(0), w(0) {}
    void set(int x) { v = x; }        /* the int overload: writes v */
    void set(Box& o) { w = o.v; }     /* the reference overload: copies o.v into w */
    int getw() { return w; }
};

int main() {
    Box a;
    Box b;
    a.set(source());   /* Box::set#1(int)  — a.v is tainted */
    b.set(a);          /* Box::set#1(Box&) — b.w = a.v */
    sink(b.getw());
    return 0;
}
