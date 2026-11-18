/* C++ reference results as locations (spec 048), POSITIVE: `set(pick(x))` with `int& pick(int& v) { return v; }`: `set`'s reference cell is copied back into `x`, the place `pick` returns. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& pick(int& v) { return v; }
void set(int& o) { o = source(); }
int main() { int x = 0; set(pick(x)); sink(x); return 0; }
