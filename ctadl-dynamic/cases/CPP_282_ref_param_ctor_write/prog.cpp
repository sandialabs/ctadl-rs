/* C++ reference parameters after #154, POSITIVE: a constructor writing its `int& o`. Construction copies the cell back into the argument, as any call does (spec 047; was known gap F8). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { int x = 0; B b(x); sink(x); return 0; }
