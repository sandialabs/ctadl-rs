/* C++ reference arguments through indirect calls and template `T&` (spec 052), NEGATIVE: `void clobber(int v) { v = source(); }` through a function pointer: a by-value callee's write to its parameter never reaches the caller's dual cell. s=none d=none. */
extern "C" int source();
extern "C" void sink(int);

void clobber(int v) { v = source(); (void)v; }
int main() { void (*fp)(int) = clobber; int x = 0; fp(x); sink(x); return 0; }
