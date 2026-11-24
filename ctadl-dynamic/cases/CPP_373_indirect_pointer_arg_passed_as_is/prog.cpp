/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `void wr(int* p) { *p = source(); }` through a function pointer with a pointer variable: a pointer is never passed as a dual cell. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

void wr(int* p) { *p = source(); }
int main() { void (*fp)(int*) = wr; int x = 0; int* p = &x; fp(p); sink(x); return 0; }
