/* C++ constructor with a member-initializer list. `Box(int x) : v(x) {}` initializes the
   member `v` from the argument `x` in its initializer list (the body is empty). CTADL
   lowers each `member(expr)` initializer as `this.member = expr` before the body, so this
   is identical to writing `v = x` in the body: `Box b(source())` taints `b.v`, and
   `b.get()` reads it back. DFSan observes the same flow; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) : v(x) {}
    int get() { return v; }
};

int main() {
    Box b(source());
    sink(b.get());
    return 0;
}
