/* C++ arrays of objects (spec 038), POSITIVE (`a4_array_method_get`). Both calls dispatch on `arr[1]`, but the taint sits at a path only the callees spell: F4 (OPEN, known gap). s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } int get() { return v; } };
int main() { B arr[2]; arr[1].set(source()); sink(arr[1].get()); return 0; }
