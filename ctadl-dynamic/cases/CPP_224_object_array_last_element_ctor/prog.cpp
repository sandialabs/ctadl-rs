/* C++ arrays of objects (spec 038), POSITIVE (`a3_array_second_element_ctor`). Every element is constructed, not just the first: `arr[2]` too. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B() : v(source()) {} };
int main() { B arr[3]; sink(arr[2].v); return 0; }
