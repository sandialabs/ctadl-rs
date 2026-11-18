/* C++ reference results as locations (spec 048), POSITIVE: `int& r = pick(x); r = source()`: `r` aliases `x`, not the call's result temp. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& pick(int& v) { return v; }
int main() { int x = 0; int& r = pick(x); r = source(); sink(x); return 0; }
