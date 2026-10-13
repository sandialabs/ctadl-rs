/* C++ lambdas capturing this (spec 040), POSITIVE (`t5_this_arrow`). `this->v = x` inside a `[this]` lambda. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; void run(int x) { auto f = [this, x]() { this->v = x; }; f(); } };
int main() { S s; s.v = 0; s.run(source()); sink(s.v); return 0; }
