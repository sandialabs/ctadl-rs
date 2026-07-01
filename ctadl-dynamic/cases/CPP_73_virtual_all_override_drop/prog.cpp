/* C++ virtual dispatch (CHA) — all overrides drop, NEGATIVE. Both `Base::get` (virtual) and
   `Derived::get` (override) return a constant (0) -- every override in the subtree drops. A
   `Derived` object used through a `Base*` calls `p->get()` virtually. CTADL's CHA target set is
   `{Base::get, Derived::get}`, but since NEITHER returns the tainted member, the union carries
   no taint -- CHA does not invent a spurious flow just from listing multiple targets. DFSan runs
   `Derived::get`, which really returns 0 (d=none). CTADL matches: s=none d=none agree. Guards
   that the multi-target edge stays precise when no override actually flows. */
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
    Base* p = &d;
    p->set(source());      /* taints the object's member v -- real taint enters */
    sink(p->get());        /* virtual: every subtree override returns 0 -- drops */
    return 0;
}
