/* C++ qualified calls and constructor forms (spec 044), POSITIVE (`s02_static_via_object`). `s.id(x)` with a static `id` did not dispatch; it is `S::id(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { static int id(int x) { return x; } };
int main() { S s; sink(s.id(source())); return 0; }
