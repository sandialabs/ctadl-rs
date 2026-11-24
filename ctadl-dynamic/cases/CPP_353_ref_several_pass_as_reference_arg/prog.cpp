/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = choose(x, y, 1); set(r); sink(x)` with `void set(int& o)`. The reference cell's copy-back writes each candidate weakly. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
void set(int& o) { o = source(); }
int main() { int x = 0; int y = 0; int& r = choose(x, y, 1); set(r); sink(x); return 0; }
