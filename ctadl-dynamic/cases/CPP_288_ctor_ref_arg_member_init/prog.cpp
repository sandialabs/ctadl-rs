/* C++ constructor reference arguments (spec 047), POSITIVE: `H(int& o) : m(o)` passes its reference cell to a member object's constructor, which writes it. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
struct H { B m; H(int& o) : m(o) {} };
int main() { int x = 0; H h(x); sink(x); return 0; }
