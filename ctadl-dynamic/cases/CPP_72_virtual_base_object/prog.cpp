/* C++ virtual dispatch (CHA) — the base-object case, POSITIVE. Mirror of CPP_71 with the
   flowing/dropping overrides swapped AND the object a `Base`: `Base::get` is `virtual` and
   returns the member (flows); `Derived::get` overrides it to drop. The object is a `Base b`,
   used through a `Base*` (`Base* p = &b`), so `p->get()` runs the dynamic type's `Base::get`
   (flows). CTADL's CHA target set for a `Base`-static-type virtual call is still
   `{Base::get, Derived::get}` (the whole subtree, over-approximating), but the union's taint is
   dominated by the flowing `Base::get`, so the flow is reported. DFSan runs exactly `Base::get`
   (d=flow). CTADL matches: s=flow d=flow agree. Together with CPP_71 this shows the union tracks
   whichever override the dynamic type actually is, in both directions. */
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
    Base* p = &b;          /* static type Base, dynamic type Base */
    p->set(source());      /* taints the object's member v -- real taint enters */
    sink(p->get());        /* virtual: runs Base::get -> returns tainted v -- flows */
    return 0;
}
