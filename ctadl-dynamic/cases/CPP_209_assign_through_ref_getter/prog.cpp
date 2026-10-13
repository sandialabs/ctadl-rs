/* C++ reference results (spec 036), POSITIVE (`r1_assign_through_ref_getter`). `b.ref() = x`, where `ref` returns the member `v` by reference, used to store into a dead temp. It stores to `b.v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int& ref() { return v; } };
int main() { B b; b.ref() = source(); sink(b.v); return 0; }
