/* C++ lambdas capturing this (spec 040), POSITIVE (`t4_this_method_call`). `set(x)` on the captured `this` is dispatched, but the member is read only by the callee: F4 (OPEN, known gap). s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; void set(int x) { v = x; } void run(int x) { auto f = [this, x]() { set(x); }; f(); } };
int main() { S s; s.v = 0; s.run(source()); sink(s.v); return 0; }
