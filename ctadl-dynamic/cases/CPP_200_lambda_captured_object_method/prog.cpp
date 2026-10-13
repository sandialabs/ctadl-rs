/* C++ lambdas (spec 034), POSITIVE (`l16_object_by_ref_method`). A captured object keeps its class, so `b.set(x)` in the body dispatches, with copy-in/copy-out through the slot. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } };
int main() { B b; b.v = 0; auto f = [&]() { b.set(source()); }; f(); sink(b.v); return 0; }
