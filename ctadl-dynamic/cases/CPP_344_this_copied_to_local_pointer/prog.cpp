/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `void run() { B* q = this; q->v = source(); }`; `b.run(); sink(b.v)`. `q = this` binds `q` to the receiver object in `addr_alias`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; void run() { B* q = this; q->v = source(); } };
int main() { B b; b.run(); sink(b.v); return 0; }
