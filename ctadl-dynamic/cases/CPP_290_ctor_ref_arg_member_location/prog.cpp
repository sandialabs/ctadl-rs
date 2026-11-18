/* C++ constructor reference arguments (spec 047), POSITIVE: `B b(p.v)` copies the written cell back into the member `p.v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int v; };
struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { P p; p.v = 0; B b(p.v); sink(p.v); return 0; }
