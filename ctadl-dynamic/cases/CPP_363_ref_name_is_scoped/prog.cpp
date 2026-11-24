/* C++ reference locals bind their locations (spec 051), NEGATIVE: `{ int& r = x; r = 1; } int r = 0; r = source(); sink(x)`. Once its block ends, a later `int r` is a plain local, not the reference. s=none d=none. */
extern "C" int source();
extern "C" void sink(int);

int main() { int x = 0; { int& r = x; r = 1; } int r = 0; r = source(); sink(x); return 0; }
