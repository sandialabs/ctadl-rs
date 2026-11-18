/* C++ constructor reference arguments (spec 047), POSITIVE: the temporary in `B(x).get()` with `B(int& o)` writing `o = source()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } int get() { return k; } };
int main() { int x = 0; int r = B(x).get(); sink(x); return r; }
