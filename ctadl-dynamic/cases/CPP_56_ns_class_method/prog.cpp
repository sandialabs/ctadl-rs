/* C++ namespaces — a class defined in a named namespace, used by qualified type name. The
   class `ns::Box` registers under its qualified name; the declaration `ns::Box b;` records
   `b`'s type as `ns::Box`, so `b.set(...)` dispatches to `ns::Box::set` and `b.get()` to
   `ns::Box::get` (each modeled with an implicit `this` by-ref). Taint flows source() ->
   b.v (via the setter's this-by-ref write) -> b.get()'s return -> sink. DFSan observes the
   flow through the two namespaced member functions; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

namespace ns {
    struct Box {
        int v;
        void set(int x) { v = x; }
        int get() { return v; }
    };
}

int main() {
    ns::Box b;
    b.set(source());
    sink(b.get());
    return 0;
}
