/* C++ templates (spec 033), POSITIVE (`t13_template_method_template`). A member template is lowered as a method (its name parses as an `identifier` inside a class body). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; template <class T> void set(T x) { v = x; } };
int main() { S s; s.set(source()); sink(s.v); return 0; }
