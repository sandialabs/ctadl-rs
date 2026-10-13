/* C++ arrays of objects (spec 038), NEGATIVE (`a6_array_distinct_negative`). The constructor taints `v`; `getw()` reads `w`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int w; B() : v(source()), w(0) {} int getw() { return w; } };
int main() { B arr[2]; sink(arr[0].getw()); return 0; }
