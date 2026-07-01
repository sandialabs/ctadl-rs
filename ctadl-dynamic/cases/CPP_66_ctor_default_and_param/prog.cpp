/* C++ constructor overloading (by arity) — default ctor + parameterized ctor.
   `Box` declares a DEFAULT constructor `Box(){v=0;}` (arity 0) and a parameterized constructor
   `Box(int x){v=x;}` (arity 1). They form an overload set of two arities keyed on `Box::Box`,
   so each lowers under a distinct arity-mangled name (`Box::Box#0`, `Box::Box#1`) — the default
   is not clobbered by the parameterized ctor. The construction `Box b(source())` has ONE
   explicit argument, so it resolves to the arity-1 constructor `Box::Box#1`, whose body writes
   `v = x` from that argument; `b.get()` reads it back. clang++ selects `Box(int)`, so DFSan
   sees the flow (`s=flow d=flow`). */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box() { v = 0; }
    Box(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box b(source());
    sink(b.get());
    return 0;
}
