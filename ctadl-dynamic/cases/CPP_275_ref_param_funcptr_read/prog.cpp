/* C++ reference parameters after #154, POSITIVE: a call through a function pointer does not pass the argument by address. A reference cell is also read from its root (the callee stores it into `.deref` on entry), so the read still flows. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int get(int& r) { return r; }
int main() { int (*fp)(int&) = get; int x = source(); sink(fp(x)); return 0; }
