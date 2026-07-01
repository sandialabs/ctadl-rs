/* C++ single inheritance — non-virtual override, NEGATIVE. `Base` defines `set`/`get`;
   `Derived` redefines `get` to return a constant (0), hiding `Base::get`. The object is used
   through its concrete static type (`Derived d`), so `d.get()` is compile-time (non-virtual)
   dispatch: it must select `Derived::get` (the receiver's own class is checked first), which
   DROPS the taint that the inherited `Base::set` wrote into `d.v`. So no taint reaches the
   sink. DFSan agrees: `Derived::get` really returns 0 at runtime (d=none). If CTADL resolved
   `d.get()` to `Base::get` instead (ignoring static-type override), it would spuriously report
   a flow (s=flow d=none). CTADL matches DFSan: s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

struct Derived : Base {
    int get() { return 0; }   /* non-virtual override: drops, hides Base::get */
};

int main() {
    Derived d;
    d.set(source());   /* inherited setter taints d.v -- real taint enters */
    sink(d.get());     /* Derived::get (static-type dispatch) returns 0 -- drops */
    return 0;
}
