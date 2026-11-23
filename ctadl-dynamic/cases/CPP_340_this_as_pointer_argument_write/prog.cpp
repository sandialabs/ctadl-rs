/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `void run() { helper(this); }`, `helper(B* p) { p->v = source(); }`; `b.run(); sink(b.v)`. `this` is a cell holding a copy of the receiver, copied back after the call. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B; void helper(B* p);
struct B { int v; void run() { helper(this); } };
void helper(B* p) { p->v = source(); }
int main() { B b; b.run(); sink(b.v); return 0; }
