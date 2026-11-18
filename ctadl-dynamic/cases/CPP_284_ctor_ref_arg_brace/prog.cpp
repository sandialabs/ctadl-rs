/* C++ constructor reference arguments (spec 047), POSITIVE: `B b{x}` (an `initializer_list`) with `B(int& o)` writing `o = source()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { int x = 0; B b{x}; sink(x); return 0; }
