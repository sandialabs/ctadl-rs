/* C++ reference parameters after #154, NEGATIVE: `set_ref(x, 1)` writes a constant into `x`; the tainted `y` never reaches it. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

void set_ref(int& out, int v) { out = v; }
int main() { int x = 0; int y = source(); set_ref(x, 1); sink(x); return y - y; }
