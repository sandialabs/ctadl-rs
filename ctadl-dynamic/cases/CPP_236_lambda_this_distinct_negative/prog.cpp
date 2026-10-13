/* C++ lambdas capturing this (spec 040), NEGATIVE (`t6_this_distinct_negative`). The lambda taints `v` and clears `w`; `w` stays clean. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int w; void run(int x) { auto f = [this, x]() { w = 0; v = x; }; f(); } };
int main() { S s; s.v = 0; s.w = 0; s.run(source()); sink(s.w); return 0; }
