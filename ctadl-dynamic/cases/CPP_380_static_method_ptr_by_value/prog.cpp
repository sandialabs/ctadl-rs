/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `void (*f)(int) = &S::use; f(source())` with `static void use(int v) { sink(v); }`. A qualified function name used as a value is a function pointer. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct S { static void use(int v) { sink(v); } };
int main() { void (*f)(int) = &S::use; f(source()); return 0; }
