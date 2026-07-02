/* C++ heap polymorphism (spec 014, FR-2), POSITIVE — the canonical `Base* p = new Derived()`
   idiom. `Base::get` is `virtual` and returns a constant (drops); `Derived::get` overrides it
   to return the member (flows). The object is a `Derived`, allocated on the heap through a
   `Base*`, so `p->get()` is a VIRTUAL call: at runtime it selects the dynamic type's
   `Derived::get`, returning the tainted member. STATIC dispatch on the pointer's declared type
   (`Base`) would call `Base::get` (drops) and MISS the flow -- unsound. CTADL models the virtual
   call by class-hierarchy analysis over the declared static type `Base`'s subtree (`Base::get`,
   `Derived::get`), a sound superset of the single dynamic target, capturing the flow through
   `Derived::get`. DFSan runs exactly `Derived::get`: s=flow d=flow agree. */
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
    Base* p = new Derived();   /* static type Base, dynamic type Derived, on the heap */
    p->set(source());          /* taints the object's member v */
    sink(p->get());            /* virtual: runs Derived::get -> returns tainted v -- flows */
    delete p;                  /* taint no-op */
    return 0;
}
