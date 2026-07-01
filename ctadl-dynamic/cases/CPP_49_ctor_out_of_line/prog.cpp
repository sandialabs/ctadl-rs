/* C++ out-of-line constructor definition. The class `Box` only *declares* its constructor
   `Box(int x);`; the body lives at top level as `Box::Box(int x){ v = x; }` (the declarator
   is a `qualified_identifier` `Box::Box`). CTADL discovers the out-of-line definition and
   lowers it with the same implicit `this` (`ByRef`) as an inline constructor, so
   `Box b(source())` calls it and taints `b.v`; `b.get()` reads it back. DFSan observes the
   flow through the out-of-line constructor; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x);
    int get() { return v; }
};

Box::Box(int x) { v = x; }

int main() {
    Box b(source());
    sink(b.get());
    return 0;
}
