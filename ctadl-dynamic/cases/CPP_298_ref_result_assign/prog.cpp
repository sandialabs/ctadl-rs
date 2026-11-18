/* C++ reference results as locations (spec 048), POSITIVE: `pick(x) = source()` stores into `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& pick(int& v) { return v; }
int main() { int x = 0; pick(x) = source(); sink(x); return 0; }
