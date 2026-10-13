/* C++ arrays of objects (spec 038), POSITIVE (`a8_pointer_index_method`). `p[0].set(x)` through a class pointer parameter. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } };
void fill(B* p) { p[0].set(source()); }
int main() { B arr[2]; fill(arr); sink(arr[0].v); return 0; }
