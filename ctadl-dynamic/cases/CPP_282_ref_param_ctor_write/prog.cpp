/* C++ reference parameters after #154, POSITIVE, KNOWN GAP F8: a constructor writing its `int& o`. Construction copies the argument into a temp cell but has no argument node to copy back into. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { int x = 0; B b(x); sink(x); return 0; }
