/* C++ virtual dispatch (CHA) through a base REFERENCE, POSITIVE (reference twin of CPP_71).
   `Base::get` is `virtual` and drops; `Derived::get` overrides and flows. A `Derived` object is
   bound to a `Base& r` (static type `Base`, dynamic type `Derived`), so `r.get()` is a VIRTUAL
   call: at runtime it selects `Derived::get`, returning the tainted member. STATIC dispatch on
   the reference's static type (`Base`) would call `Base::get` (drops) and MISS the flow --
   unsound. CTADL models the virtual call by class-hierarchy analysis: a multi-target
   `DirectCall` over `Base`'s subtree (`Base::get`, `Derived::get`) with the referent as arg 0,
   capturing the flow through `Derived::get`. DFSan runs exactly `Derived::get` (d=flow). CTADL
   matches: s=flow d=flow agree. The reference twin of CPP_71 -- 012's machinery already handles
   `.`-on-a-reference the same as `->`-on-a-pointer; this locks that in. */
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
    Base& r = d;           /* static type Base, dynamic type Derived */
    r.set(source());       /* taints the object's member v -- real taint enters */
    sink(r.get());         /* virtual: runs Derived::get -> returns tainted v -- flows */
    return 0;
}
