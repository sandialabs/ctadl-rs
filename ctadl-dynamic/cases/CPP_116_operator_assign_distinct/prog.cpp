/* C++ operator overloading (spec 023, FR-2), NEGATIVE — field sensitivity across the operator
   call (`p2f_operator_assign_distinct`). `operator=` writes only `v`; the sink reads the member
   `w`, which nothing ever writes. Dispatching `a = source()` to `Acc::operator=(&a, t)` passes
   `a` by reference, so the body's `store @p0.v` lands at `a.v` and at no other path — `a.w` stays
   clean. This case goes s=flow (a false positive) the moment the operator call conflates the
   whole receiver object instead of writing the member the body names, which is exactly the
   failure a by-ref receiver plus field-sensitive stores must not have. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Acc {
    int v;
    int w;
    Acc() : v(0), w(0) {}
    Acc& operator=(int x) { v = x; return *this; }   /* writes `v` only */
    int getw() { return w; }
};

int main() {
    Acc a;
    a = source();          /* taints a.v */
    sink(a.getw());        /* reads a.w — never written */
    return 0;
}
