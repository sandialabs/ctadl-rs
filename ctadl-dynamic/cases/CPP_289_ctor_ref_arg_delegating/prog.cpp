/* C++ constructor reference arguments (spec 047), POSITIVE: the delegating `S(int& o) : S(o, 0)` passes its reference cell to the target constructor, which writes it. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int k; S(int& o, int n) : k(n) { o = source(); } S(int& o) : S(o, 0) {} };
int main() { int x = 0; S s(x); sink(x); return 0; }
