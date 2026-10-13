/* C++ temporaries (spec 027), POSITIVE (`t2_return_temp`): `return B(source());` returns the
   constructed temporary, and the caller's copy reads its member. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int x) : v(x) {} int get() { return v; } };
B make() { return B(source()); }
int main() { B c = make(); sink(c.get()); return 0; }
