/* C++ default construction (spec 028), POSITIVE (`n1_default_ctor_decl`). `B b;` never ran the
   user default constructor: the `construct` hook declined a bare `identifier` declarator, so the
   object stayed unconstructed. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B() : v(source()) {} int get() { return v; } };
int main() { B b; sink(b.get()); return 0; }
