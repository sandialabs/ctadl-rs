/* C++ operator overloading (spec 023, FR-2), POSITIVE — the probed soundness gap
   (`p2e_operator_plus_assign`). A COMPOUND assignment to an object is the same method call as a
   plain one, under the operator's own spelling: `a += source()` is `a.operator+=(source())`. Note
   what the call does NOT carry — the built-in compound lowering reads the target's old value as a
   second operand, but `operator+=` reads it itself, through `this` (`v = v + x`), so the call
   takes only the right-hand side. Before this slice the expression lowered as the built-in
   read-modify-write on `a` and the operator body never ran: s=none against DFSan's d=flow.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Acc {
    int v;
    Acc() : v(0) {}
    Acc& operator+=(int x) { v = v + x; return *this; }   /* reads the old `v` through `this` */
    int get() { return v; }
};

int main() {
    Acc a;
    a += source();         /* Acc::operator+=(&a, source()) — one argument, not two */
    sink(a.get());
    return 0;
}
