/* C++ implicit member calls (spec 035), POSITIVE (`i1_instance_call`). `get()` inside a method used to be a direct call to a nonexistent free `get` with no receiver. It is `this->get()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct S { int v; int get() { return v; } int twice() { return get() + get(); } };
int main() { S s; s.v = source(); sink(s.twice()); return 0; }
