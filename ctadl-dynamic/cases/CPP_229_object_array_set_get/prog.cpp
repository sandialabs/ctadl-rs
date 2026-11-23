/* C++ arrays of objects (spec 038), POSITIVE (`a4_array_method_get`), formerly F4. Both calls dispatch on `arr[1]`; since spec 050 the receiver's write-back stores each member by name, which spells the element's member path. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } int get() { return v; } };
int main() { B arr[2]; arr[1].set(source()); sink(arr[1].get()); return 0; }
