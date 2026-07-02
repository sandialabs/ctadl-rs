/* C++ virtual dispatch (CHA) through a base REFERENCE, field-sensitivity NEGATIVE. A `virtual`
   setter `setv` (overridden in `Derived`) taints the member `v`; the sink reads a *distinct*
   member `w` (set to a constant 0). The virtual `r.setv(source())` is dispatched by CHA over the
   subtree, but it writes only `v`; `v` and `w` are separate field-named access paths, so the
   taint never crosses to `w`. DFSan agrees: w's shadow stays clean (d=none). CTADL matches:
   s=none d=none agree -- field sensitivity holds through a virtual call on a reference. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    int w;
    virtual void setv(int x) { v = x; }   /* virtual setter, taints v */
    int getw() { return w; }
};

struct Derived : Base {
    void setv(int x) override { v = x; }  /* override, also taints only v */
};

int main() {
    Derived d;
    d.w = 0;               /* a distinct member, untainted */
    Base& r = d;
    r.setv(source());      /* virtual: taints d.v (either override) -- real taint enters */
    sink(r.getw());        /* reads d.w -- distinct, untainted */
    return 0;
}
