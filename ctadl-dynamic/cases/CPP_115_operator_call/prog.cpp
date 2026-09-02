/* C++ operator overloading (spec 023, FR-3), POSITIVE — the probed soundness gap
   (`p2c_operator_call`). `f(1)` where `f` names an OBJECT (not a function) is
   `f.operator()(1)`. Told apart from a real call by exactly one neutral question: does the callee
   identifier resolve to a known class object? Before this slice it did not ask, so `f(1)` lowered
   as a call to an undefined function `f`, `define_extern_functions` invented an empty body for it,
   and an empty body returns nothing — the taint in `f.k` stopped there (s=none against DFSan's
   d=flow). Now it dispatches to `Fn::operator()` with `f` as the by-ref receiver and the body's
   `k + y` carries the member out. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Fn {
    int k;
    Fn() : k(0) {}
    void set(int x) { k = x; }
    int operator()(int y) { return k + y; }   /* the method `f(1)` really calls */
};

int main() {
    Fn f;
    f.set(source());
    sink(f(1));            /* Fn::operator()(&f, 1) — not an extern stub call to `f` */
    return 0;
}
