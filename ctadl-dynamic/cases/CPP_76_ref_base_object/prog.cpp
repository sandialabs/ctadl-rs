/* C++ virtual dispatch (CHA) through a base REFERENCE, base-object case, POSITIVE (reference
   twin of CPP_72). `Base::get` is `virtual` and flows; `Derived::get` overrides and drops. The
   object is a `Base b` bound to `Base& r`, so `r.get()` runs the dynamic type's `Base::get`
   (flows). CTADL's CHA target set for a `Base`-static-type virtual call is still
   `{Base::get, Derived::get}` (the whole subtree), but the union's taint is dominated by the
   flowing `Base::get`, so the flow is reported. DFSan runs exactly `Base::get` (d=flow). CTADL
   matches: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    virtual int get() { return v; }   /* virtual, flows */
};

struct Derived : Base {
    int get() override { return 0; }  /* override, drops */
};

int main() {
    Base b;
    Base& r = b;           /* static type Base, dynamic type Base */
    r.set(source());       /* taints the object's member v -- real taint enters */
    sink(r.get());         /* virtual: runs Base::get -> returns tainted v -- flows */
    return 0;
}
