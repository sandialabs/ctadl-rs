/* C++ operator overloading (spec 023, FR-1/FR-2), POSITIVE — the probed soundness gap
   (`p2a_operator_assign`). An overloaded operator is a method with a punctuation name:
   `a = source()` on an object whose class declares `operator=` is `a.operator=(source())`, not a
   store into `a`. Before this slice the frontend did BOTH halves wrong — `cpp_collect_methods`
   required a `field_identifier` declarator, so the `operator_name`-declared method was never even
   lowered, and the shared `assignment_expression` arm lowered the expression as the built-in
   value copy into `a`. Taint therefore never reached the member the operator body writes and the
   case was s=none against DFSan's d=flow. Now `Acc::operator=` is discovered with its implicit
   `this` and the assignment lowers to `DirectCall Acc::operator=(&a, t)`, so `v = x` writes back
   through the by-ref receiver and `a.get()` reads it. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Acc {
    int v;
    Acc() : v(0) {}
    Acc& operator=(int x) { v = x; return *this; }   /* the method `a = …` really calls */
    int get() { return v; }
};

int main() {
    Acc a;
    a = source();          /* Acc::operator=(&a, source()) — not a store into `a` */
    sink(a.get());
    return 0;
}
