/* C++ heap object with a constructor argument (spec 014, FR-1/FR-4), POSITIVE. `new
   Box(source())` runs `Box::Box(int)` on the synthetic heap object, so the tainted argument
   lands in member `v`; the pointer `p` aliases that object and `sink(p->get())` reads `v` back
   out. `delete p` is a taint no-op. CTADL reuses its constructor lowering (specs 006/010) on
   the heap object, so the ctor argument's taint reaches the member exactly as for a stack
   object. DFSan observes the same flow: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) { v = x; }   /* ctor sets member from argument */
    int get() { return v; }
};

int main() {
    Box* p = new Box(source());   /* ctor taints the heap object's member v */
    sink(p->get());               /* reads the tainted member back -- flows */
    delete p;                     /* taint no-op */
    return 0;
}
