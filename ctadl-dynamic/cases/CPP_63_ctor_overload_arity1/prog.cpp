/* C++ constructor overloading (by arity) — POSITIVE (arity 1).
   `Box` declares TWO constructors of different arity: `Box(int x){v=x;}` and
   `Box(int x,int y){v=y;}`. They form an overload set keyed on `Box::Box`, so each lowers
   under an arity-mangled ctor name (`Box::Box#1`, `Box::Box#2`) — neither clobbers the other.
   The construction `Box b(source())` has ONE explicit argument, so it resolves to the arity-1
   constructor `Box::Box#1`, whose body writes `v = x` from that argument; `b.get()` reads it
   back. This pins arity-1 selection and that taint follows exactly the selected ctor's body.
   clang++ selects `Box(int)` at construction, so DFSan sees the flow (`s=flow d=flow`). */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) { v = x; }
    Box(int x, int y) { v = y; }
    int get() { return v; }
};

int main() {
    Box b(source());
    sink(b.get());
    return 0;
}
