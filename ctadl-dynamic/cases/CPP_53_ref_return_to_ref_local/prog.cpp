/* C++ reference-returning method bound to a reference local. `Box::setV` returns `Box&`
   and `return *this`, so `b.setV(source())`'s result aliases `b`. Binding it to `Box& r`
   makes `r` an alias of `b` (not a copy of the returned temporary), so `r.getV()` reads
   `b.v` — the member `setV` tainted from `source()`. DFSan observes source() -> b.v ->
   r.getV() -> sink; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box& setV(int x) { v = x; return *this; }
    int getV() { return v; }
};

int main() {
    Box b;
    Box& r = b.setV(source());
    sink(r.getV());
    return 0;
}
