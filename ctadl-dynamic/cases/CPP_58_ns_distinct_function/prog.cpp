/* C++ namespaces — NEGATIVE: qualified calls resolve precisely, not to a same-scope sibling.
   The namespace `ns` declares two distinct free functions: `keep(x)` returns its argument and
   `drop(x)` discards it (returns 0). The call `ns::drop(source())` must resolve to `ns::drop`
   (which drops the taint), NOT to `ns::keep` — so no taint reaches the sink. This pins that a
   qualified callee string resolves to the same-named qualified definition and never cross-
   resolves to a sibling. DFSan sees no flow (drop discards its arg); CTADL must match (`s=none
   d=none`). A spurious ns::drop->ns::keep resolution would be a soundness-side false positive. */
extern "C" int source();
extern "C" void sink(int);

namespace ns {
    int keep(int x) { return x; }
    int drop(int x) { return 0; }
}

int main() {
    sink(ns::drop(source()));
    return 0;
}
