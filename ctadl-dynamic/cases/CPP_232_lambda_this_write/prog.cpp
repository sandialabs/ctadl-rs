/* C++ lambdas capturing this (spec 040), POSITIVE (`t1_this_capture_write`). `[this]() { v = source(); }`: members in the body resolve through the `this` slot, and the method stores the slot back into its object after its calls. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; void run() { auto f = [this]() { v = source(); }; f(); } };
int main() { S s; s.v = 0; s.run(); sink(s.v); return 0; }
