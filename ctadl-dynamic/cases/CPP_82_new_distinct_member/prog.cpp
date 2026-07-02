/* C++ heap object field sensitivity (spec 014, FR-4), NEGATIVE. The setter taints member `a`
   on a heap object; the sink reads a distinct member `b` (set to 0). Real taint enters the
   program (`p->a`), but a field-sensitive model must not leak `a`'s taint into the distinct
   member `b` just because both live in the same heap object. `delete p` is a taint no-op. CTADL
   reports no `a` -> `b` flow, and DFSan observes none at runtime: s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int a;
    int b;
    void seta(int x) { a = x; }
    int getb() { return b; }
};

int main() {
    Box* p = new Box();    /* heap-allocated object; p aliases it */
    p->b = 0;              /* distinct member b holds a constant (no label) */
    p->seta(source());     /* taints member a only */
    sink(p->getb());       /* reads distinct member b -- no flow */
    delete p;              /* taint no-op */
    return 0;
}
