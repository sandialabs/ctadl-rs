/* C++ arrays of objects (spec 038), POSITIVE (`a1_array_ctor`). `B arr[2];` used to construct nothing; each element now runs `B::B`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B() : v(source()) {} };
int main() { B arr[2]; sink(arr[0].v); return 0; }
