/* C++ reference parameters after #154, POSITIVE: `swp(int& a, int& b)` moves the tainted `x` into `y`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void swp(int& a, int& b) { int t = a; a = b; b = t; }
int main() { int x = source(); int y = 0; swp(x, y); sink(y); return 0; }
