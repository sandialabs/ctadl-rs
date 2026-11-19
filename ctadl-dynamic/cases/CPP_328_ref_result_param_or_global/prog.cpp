/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& pg(int& a, int c) { if (c) return a; return g; }`; `pg(x, 0) = source(); sink(g)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int g = 0;
int& pg(int& a, int c) { if (c) return a; return g; }
int main() { int x = 0; pg(x, 0) = source(); sink(g); return 0; }
