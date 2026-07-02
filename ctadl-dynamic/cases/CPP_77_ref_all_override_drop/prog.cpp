/* C++ virtual dispatch (CHA) through a base REFERENCE, NEGATIVE (reference twin of CPP_73).
   Both `Base::get` (virtual) and `Derived::get` (override) return a constant -- every override
   in the subtree DROPS its object. A `Derived` object is bound to `Base& r`; `r.get()` is a
   virtual call whose CHA target set is `{Base::get, Derived::get}`, but neither flows, so no
   taint reaches the sink even though `r.set(source())` taints the object. DFSan runs
   `Derived::get` (drops) -> d=none. CTADL matches: s=none d=none agree -- CHA over-approximation
   does not fabricate a flow when all subtree overrides drop. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    virtual int get() { return 0; }   /* virtual, drops */
};

struct Derived : Base {
    int get() override { return 0; }  /* override, also drops */
};

int main() {
    Derived d;
    Base& r = d;
    r.set(source());       /* taints the object's member v -- real taint enters */
    sink(r.get());         /* virtual: every override drops -> none reaches the sink */
    return 0;
}
