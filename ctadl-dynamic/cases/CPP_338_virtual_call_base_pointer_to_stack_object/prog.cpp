/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `D d; Base* b = &d; b->set(source()); sink(d.v)`, `set` virtual. CHA dispatch on `*b`, d's storage. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct Base { int v; virtual void set(int x) { v = x; } };
struct D : public Base { void set(int x) override { v = x + 1; } };
int main() { D d; Base* b = &d; b->set(source()); sink(d.v); return 0; }
