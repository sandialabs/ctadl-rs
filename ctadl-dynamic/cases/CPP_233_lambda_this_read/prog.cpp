/* C++ lambdas capturing this (spec 040), POSITIVE (`t2_this_capture_read`). A `[this]` lambda reading a member through the slot. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; void run() { auto f = [this]() { sink(v); }; f(); } };
int main() { S s; s.v = source(); s.run(); return 0; }
