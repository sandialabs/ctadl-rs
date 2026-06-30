/* C++ pointer receiver. `p` points to `b` (`Box* p = &b`), and the method calls are written
   through the pointer: `p->set(source())` / `p->get()`. CTADL dispatches each to `Box::set`/
   `Box::get` with the pointed-to object as the arg-0 (`ByRef`) receiver, so the setter's write
   to `this.v` propagates back and the getter reads it out. DFSan sees the same flow through
   `*p` and CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box b;
    Box* p = &b;
    p->set(source());
    sink(p->get());
    return 0;
}
