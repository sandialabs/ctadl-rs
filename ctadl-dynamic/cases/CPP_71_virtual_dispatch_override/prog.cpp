/* C++ virtual dispatch (CHA) — the override case, POSITIVE. `Base::get` is `virtual` and
   returns a constant (drops); `Derived::get` overrides it to return the member (flows). The
   object is a `Derived`, but it is used through a `Base*` (`Base* p = &d`), so `p->get()` is a
   VIRTUAL call: at runtime it selects the dynamic type's `Derived::get`, which returns the
   tainted member. STATIC dispatch on the pointer's static type (`Base`) would call `Base::get`
   (drops) and MISS the flow -- unsound (s=none d=flow). CTADL models the virtual call by
   class-hierarchy analysis: a multi-target `DirectCall` listing every override in `Base`'s
   subtree (`Base::get`, `Derived::get`), a sound superset of the single dynamic target, so the
   flow through `Derived::get` is captured. DFSan runs exactly `Derived::get` (d=flow). CTADL
   matches: s=flow d=flow agree. This is the case that proves virtual dispatch. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    virtual int get() { return 0; }   /* virtual, drops */
};

struct Derived : Base {
    int get() override { return v; }  /* override, flows */
};

int main() {
    Derived d;
    Base* p = &d;          /* static type Base, dynamic type Derived */
    p->set(source());      /* taints the object's member v -- real taint enters */
    sink(p->get());        /* virtual: runs Derived::get -> returns tainted v -- flows */
    return 0;
}
