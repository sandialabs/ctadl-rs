/* C++ reference results (spec 036), POSITIVE (`r8_this_arrow_ref_getter`). `ref() = x` inside a method: an implicit this-call whose reference result is a location. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int& ref() { return this->v; } void put(int x) { ref() = x; } };
int main() { B b; b.v = 0; b.put(source()); sink(b.v); return 0; }
