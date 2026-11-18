/* C++ constructor reference arguments (spec 047), POSITIVE: `B b(a[1])` is a most-vexing parse (`abstract_array_declarator`); the cell is copied back into the element `a[1]`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { int a[2] = {0, 0}; B b(a[1]); sink(a[1]); return 0; }
