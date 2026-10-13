/* C++ member-object receivers (spec 030), POSITIVE (`m5_nested_member_method`): `c.b.a.set(x)`, a
   member object of a member object. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct A { int v; A() : v(0) {} void set(int x) { v = x; } };
struct B { A a; }; struct C { B b; };
int main() { C c; c.b.a.set(source()); sink(c.b.a.v); return 0; }
