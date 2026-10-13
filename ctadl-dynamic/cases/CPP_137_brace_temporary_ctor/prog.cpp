/* C++ temporaries (spec 027), POSITIVE (`t10_brace_temp`): `B{source()}` on a class with a
   constructor is a construction (a `compound_literal_expression` the hook now constructs), not an
   aggregate. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int x) : v(x) {} int get() { return v; } };
int main() { sink(B{source()}.get()); return 0; }
