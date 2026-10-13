/* C++ implicit member calls (spec 035), POSITIVE (`i9_free_function_same_name_hidden`). A member `get` hides the free `get` inside the class (C++ name lookup). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int get() { return 0; }
struct S { int v; int get() { return v; } int f() { return get(); } };
int main() { S s; s.v = source(); sink(s.f()); return 0; }
