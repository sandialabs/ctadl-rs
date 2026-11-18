/* C++ reference results as locations (spec 048), POSITIVE: `int& outer(int& v) { return pick(v); }`; `outer(x) = source()`, the place closed through `pick`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& pick(int& v) { return v; }
int& outer(int& v) { return pick(v); }
int main() { int x = 0; outer(x) = source(); sink(x); return 0; }
