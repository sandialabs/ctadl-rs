/* C++ reference receiver. `r` is a reference bound to `b` (`Box& r = b`), and the method
   calls go through it: `r.set(source())` / `r.get()`. The reference local aliases `b` (spec
   004), so CTADL dispatches each call to `Box::set`/`Box::get` with the referent `b` as the
   arg-0 (`ByRef`) receiver — the setter's member write propagates back and the getter reads
   it out. DFSan sees `r` and `b` share storage, so the flow agrees. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box b;
    Box& r = b;
    r.set(source());
    sink(r.get());
    return 0;
}
