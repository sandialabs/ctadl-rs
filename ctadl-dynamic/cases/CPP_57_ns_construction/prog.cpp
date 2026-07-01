/* C++ namespaces — construction of a namespaced class at a declaration. `ns::Box` declares
   a constructor `Box(int x)` whose body writes `v = x`. The declaration `ns::Box b(source())`
   lowers to a `DirectCall ns::Box::ns::Box(&b, source())` (the constructor is modeled with an
   implicit `this` by-ref), so the constructor's `this.v = x` write lands in `b.v`; `b.get()`
   reads it back. Taint flows source() -> b.v -> sink. DFSan observes the flow through the
   namespaced constructor; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

namespace ns {
    struct Box {
        int v;
        Box(int x) { v = x; }
        int get() { return v; }
    };
}

int main() {
    ns::Box b(source());
    sink(b.get());
    return 0;
}
