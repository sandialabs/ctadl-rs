/* C++ heap object via `new` (spec 014, FR-1/FR-4), POSITIVE. `new Box()` allocates a heap
   object with no constructor; the pointer `p` aliases it, so `p->set(source())` taints the
   object's member `v` and `sink(p->get())` reads it back out. `delete p` destroys the object
   and is a taint no-op. A heap object behaves exactly like a stack object for field-named
   taint (only its storage lives on the heap), so CTADL models `new Box()` as an anonymous
   constructed object the pointer aliases and captures the flow. DFSan observes the same flow
   at runtime: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box* p = new Box();    /* heap-allocated object; p aliases it */
    p->set(source());      /* taints the object's member v */
    sink(p->get());        /* reads the tainted member back -- flows */
    delete p;              /* taint no-op */
    return 0;
}
