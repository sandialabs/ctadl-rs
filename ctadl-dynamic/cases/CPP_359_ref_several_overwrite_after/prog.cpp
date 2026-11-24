/* C++ reference locals bind their locations (spec 051), NEGATIVE: `int& r = choose(x, y, 1); r = source(); x = 0; sink(x)`. A direct strong write after the weak one clears `x`. s=none d=none. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; int& r = choose(x, y, 1); r = source(); x = 0; sink(x); return 0; }
