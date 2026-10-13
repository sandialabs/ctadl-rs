/* C++ lambdas capturing this (spec 040), POSITIVE (`t3_implicit_this_default_ref`). `[&]` whose body names a member captures `this` implicitly. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; void run(int x) { auto f = [&]() { v = x; }; f(); } };
int main() { S s; s.v = 0; s.run(source()); sink(s.v); return 0; }
