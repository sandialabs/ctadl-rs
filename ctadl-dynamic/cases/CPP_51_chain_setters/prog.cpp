/* C++ method chaining — fluent setters. `Box::setV`/`setW` return `Box&` and `return
   *this`, so CTADL models each call's result as an *alias* to the receiver object (arg 0),
   not a copy. The chain `b.setV(source()).setW(0)` therefore dispatches both setters on the
   same object `b`: `setV` writes `b.v = source()` (via the `this`-by-ref writeback), `setW`
   writes `b.w = 0`. The terminal `b.getV()` reads `b.v` back out. DFSan observes the flow
   source() -> b.v -> sink; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box& setV(int x) { v = x; return *this; }
    Box& setW(int x) { w = x; return *this; }
    int getV() { return v; }
};

int main() {
    Box b;
    b.setV(source()).setW(0);
    sink(b.getV());
    return 0;
}
