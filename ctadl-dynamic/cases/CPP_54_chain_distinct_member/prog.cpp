/* C++ method chaining — field-sensitivity through a chain (negative). The chain
   `b.setV(source()).setW(0)` taints `b.v` from `source()` and sets `b.w = 0` (a constant).
   The sink reads a *different* member, `b.getW()` == `b.w`, which is never tainted. A
   field-sensitive model must not leak `b.v`'s taint into `b.w` just because both writes
   happen on the same chained object. DFSan observes no flow to the sink; CTADL matches
   (`s=none d=none`) — no soundness-disagree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    Box& setV(int x) { v = x; return *this; }
    Box& setW(int x) { w = x; return *this; }
    int getW() { return w; }
};

int main() {
    Box b;
    b.setV(source()).setW(0);
    sink(b.getW());
    return 0;
}
