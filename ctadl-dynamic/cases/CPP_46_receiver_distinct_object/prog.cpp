/* Negative control for receivers: tainting one object must not taint a different one. `p`
   points to `a`, and `p->set(source())` taints `a.v` through the pointer receiver — real
   taint genuinely flows into the program. But the sink reads `b.v`, a *separate* Box object
   that was never written. CTADL's method model is per-object (the receiver is arg-0 by-ref,
   and `a`, `b`, `p` are distinct locals), so it reports no `a`->`b` cross-taint (s=none);
   DFSan agrees, since `b`'s shadow stays clean (d=none). If receivers were modeled as a
   single shared object, this would risk a spurious flow. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box a;
    Box b;
    Box* p = &a;
    p->set(source());   /* taints a.v (via p) -- real taint exists */
    sink(b.v);          /* reads a different object -> still clean */
    return 0;
}
