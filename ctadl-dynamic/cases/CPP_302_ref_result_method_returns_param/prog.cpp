/* C++ reference results as locations (spec 048), POSITIVE: `int& S::pass(int& o) { return o; }`; `s.pass(x) = source()` stores into `x`. Spec 036 used to record `o` as a returned member. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int& pass(int& o) { return o; } };
int main() { S s; int x = 0; s.pass(x) = source(); sink(x); return 0; }
