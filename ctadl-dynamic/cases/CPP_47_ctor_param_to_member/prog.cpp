/* C++ constructor — argument flows into a member through the constructor body. The class
   `Box` declares a constructor `Box(int x)` whose body writes `v = x`. CTADL models the
   constructor as `Box::Box(this: ByRef, x)`, and the construction `Box b(source())` lowers
   to a `DirectCall Box::Box(&b, source())`, so the constructor's `this.v = x` write lands
   in `b.v`; `b.get()` then reads it back. (tree-sitter parses the direct-init `Box b(...)`
   as a function declaration — the "most vexing parse" — and the frontend reconstructs the
   construction from it.) DFSan observes the flow through the constructor; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box b(source());
    sink(b.get());
    return 0;
}
