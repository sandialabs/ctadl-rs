/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `B* self() { return this; }`; `b.v = source(); B* q = b.self(); sink(q->v)`. The returned cell holds a copy of the receiver. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B* self() { return this; } };
int main() { B b; b.v = source(); B* q = b.self(); sink(q->v); return 0; }
