/* C++ reference results as locations (spec 048), POSITIVE: `s.set(pick(x))`: a method's reference cell is copied back through `pick`'s result into `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& pick(int& v) { return v; }
struct S { void set(int& o) { o = source(); } };
int main() { S s; int x = 0; s.set(pick(x)); sink(x); return 0; }
