/* C++ reference results through pointers and with several places (spec 049), NEGATIVE: `rec` as in CPP_324; `x = source(); rec(x, 3) = 0; sink(x)`: one place, so a strong store that kills the taint. It used to be a precision gap. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int& rec(int& v, int n) { if (n) return rec(v, n - 1); return v; }
int main() { int x = source(); rec(x, 3) = 0; sink(x); return 0; }
