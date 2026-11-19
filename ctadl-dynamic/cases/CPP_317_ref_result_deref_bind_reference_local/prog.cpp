/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& deref(int* p) { return *p; }`; `int& r = deref(&x); r = source()`: `r` aliases `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& deref(int* p) { return *p; }
int main() { int x = 0; int& r = deref(&x); r = source(); sink(x); return 0; }
