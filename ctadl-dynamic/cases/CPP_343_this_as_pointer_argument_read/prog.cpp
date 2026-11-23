/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `int look() { return peek(this); }`, `peek(B* p)` reads `p->v`; `b.v = source(); sink(b.look())`. The copy in `this`'s cell carries the receiver's members. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B; int peek(B* p);
struct B { int v; int look() { return peek(this); } };
int peek(B* p) { return p->v; }
int main() { B b; b.v = source(); sink(b.look()); return 0; }
