/* C++ arrays of objects (spec 038), POSITIVE (`a5_pointer_decl_then_assign`). `B* p; p = &b; p->set(x)`: a class pointer declared without an initializer is now a receiver. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } };
int main() { B b; B* p; p = &b; p->set(source()); sink(b.v); return 0; }
