/* C++ subobject construction (spec 029), POSITIVE (`s8_two_level_bases`): `C : B : A`, where only
   `A` sets the tainted member. The implicit `C::C` runs `B::B`, which runs `A::A` first.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct A { int a; A() : a(source()) {} };
struct B : A { int b; B() : b(0) {} };
struct C : B { int c; };
int main() { C c; sink(c.a); return 0; }
