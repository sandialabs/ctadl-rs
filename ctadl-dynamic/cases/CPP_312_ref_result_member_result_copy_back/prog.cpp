/* C++ reference results as locations (spec 048), POSITIVE: `set(b.ref())` with spec 036's `int& ref() { return v; }`: `set`'s cell is copied back into `b.v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int w; int& ref() { return v; } };
void set(int& o) { o = source(); }
int main() { B b; b.v = 0; b.w = 0; set(b.ref()); sink(b.v); return 0; }
