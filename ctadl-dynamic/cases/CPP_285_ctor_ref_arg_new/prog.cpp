/* C++ constructor reference arguments (spec 047), POSITIVE: `new B(x)` with `B(int& o)` writing `o = source()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { int x = 0; B* p = new B(x); sink(x); delete p; return 0; }
