/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `struct S { static void put(int& o) { o = source(); } }`; `void (*f)(int&) = &S::put; f(x); sink(x)`. A qualified function name is a function pointer, and the indirect call passes `x` as a dual cell. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct S { static void put(int& o) { o = source(); } };
int main() { int x = 0; void (*f)(int&) = &S::put; f(x); sink(x); return 0; }
