/* C++ virtual dispatch (CHA) — field sensitivity through virtual dispatch, NEGATIVE. A virtual
   setter `seta` taints member `a`; a virtual getter `getb` reads a DISTINCT member `b` (set to a
   constant 0). Both methods are called virtually through a `Base*` at a `Derived` object, so each
   lowers to a multi-target `DirectCall` over the subtree. Even so, `a` and `b` are separate
   field-named access paths: the taint `seta` writes into `a` never crosses to `b`, so `getb`
   returns untainted. DFSan agrees: b's shadow stays clean (d=none). If the virtual multi-target
   edge smeared taint across the whole object, this would risk a spurious flow. CTADL matches
   DFSan: s=none d=none agree -- field sensitivity survives CHA dispatch. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int a;
    int b;
    virtual void seta(int x) { a = x; }
    virtual int getb() { return b; }
};

struct Derived : Base {
    void seta(int x) override { a = x; }
    int getb() override { return b; }
};

int main() {
    Derived d;
    d.b = 0;               /* a distinct member, untainted */
    Base* p = &d;
    p->seta(source());     /* virtual: taints member a -- real taint enters */
    sink(p->getb());       /* virtual: reads member b -- distinct, untainted */
    return 0;
}
