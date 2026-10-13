/* C++ reference results (spec 036), POSITIVE (`r3_bind_ref_to_getter`). `int& r = b.ref()` binds `r` to `b.v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int& ref() { return v; } };
int main() { B b; b.v = 0; int& r = b.ref(); r = source(); sink(b.v); return 0; }
