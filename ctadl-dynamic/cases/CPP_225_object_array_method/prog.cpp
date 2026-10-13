/* C++ arrays of objects (spec 038), POSITIVE (`a2_array_method`). `arr[0].set(x)` used not to dispatch: an array element was no receiver. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } };
int main() { B arr[2]; arr[0].set(source()); sink(arr[0].v); return 0; }
