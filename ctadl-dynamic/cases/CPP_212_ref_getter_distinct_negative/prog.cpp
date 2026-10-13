/* C++ reference results (spec 036), NEGATIVE (`r5_ref_getter_distinct_negative`). `ref` returns `w`; a store through it must not reach `v`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int w; int& ref() { return w; } };
int main() { B b; b.v = 0; b.w = 0; b.ref() = source(); sink(b.v); return 0; }
