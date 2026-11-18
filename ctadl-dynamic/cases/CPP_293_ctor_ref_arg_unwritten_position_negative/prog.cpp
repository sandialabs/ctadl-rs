/* C++ constructor reference arguments (spec 047), NEGATIVE: `B(int& a, int& o)` writes only `o`; `B b(x, y); sink(x)` stays clean. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& a, int& o) : k(0) { o = source(); } };
int main() { int x = 0; int y = 0; B b(x, y); sink(x); return 0; }
