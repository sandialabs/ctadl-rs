/* C++ non-const lvalue reference out-param: `set_ref(int& out, int v)` writes the tainted
   value through the reference, so the caller's `x` becomes tainted. A `T&` aliases the
   caller's argument (writes propagate back) — CTADL models `out` as `ByRef`, exactly like a
   pointer out-param (CPP_15), and the existing out-param propagation carries the write back.
   DFSan observes the write through the reference at the caller's `x`. */
extern "C" int source();
extern "C" void sink(int);

void set_ref(int& out, int v) {
    out = v;
}

int main() {
    int x = 0;
    set_ref(x, source());
    sink(x);
    return 0;
}
