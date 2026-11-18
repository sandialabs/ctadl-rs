/* C++ reference results as locations (spec 048), NEGATIVE: `int& second(int& a, int& b) { return b; }`; `second(x, y) = source(); sink(x)`: only `y` is written. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int& second(int& a, int& b) { return b; }
int main() { int x = 0; int y = 0; second(x, y) = source(); sink(x); return 0; }
