/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& rec(int& v, int n) { if (n) return rec(v, n - 1); return v; }`; `rec(x, 3) = source(); sink(x)`. The least fixed point gives `rec` the single place `v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& rec(int& v, int n) { if (n) return rec(v, n - 1); return v; }
int main() { int x = 0; rec(x, 3) = source(); sink(x); return 0; }
