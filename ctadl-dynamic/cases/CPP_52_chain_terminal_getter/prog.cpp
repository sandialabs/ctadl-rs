/* C++ method chaining — a terminal getter on the chained object. `Box::setV` returns
   `Box&` and `return *this`, so `b.setV(source())`'s result aliases `b`; the chained
   `.getV()` therefore reads `b.v` — the very member `setV` just wrote from `source()`. The
   whole expression `b.setV(source()).getV()` flows source() -> b.v -> return -> sink. DFSan
   observes the flow; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box& setV(int x) { v = x; return *this; }
    int getV() { return v; }
};

int main() {
    Box b;
    sink(b.setV(source()).getV());
    return 0;
}
