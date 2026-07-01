/* C++ single (non-virtual) inheritance — inherited methods. `Derived` adds nothing of its
   own; `set`/`get` are defined in `Base`. CTADL records `Derived`'s base and, on a `Derived`
   receiver, walks the base chain to dispatch `d.set(...)`/`d.get()` to `Base::set`/`Base::get`
   with `d` as the by-ref receiver. `Base::set` writes `this.v = source()` (== `d.v`); the
   inherited `Base::get` reads `d.v` back. DFSan observes source() -> d.v -> sink; CTADL
   matches (s=flow d=flow agree). */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

struct Derived : Base {};

int main() {
    Derived d;
    d.set(source());   /* inherited setter taints d.v */
    sink(d.get());     /* inherited getter reads d.v back */
    return 0;
}
