/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& tern(int& a, int& b, int c) { return c ? a : b; }`; `tern(x, y, 1) = source(); sink(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& tern(int& a, int& b, int c) { return c ? a : b; }
int main() { int x = 0; int y = 0; tern(x, y, 1) = source(); sink(x); return 0; }
