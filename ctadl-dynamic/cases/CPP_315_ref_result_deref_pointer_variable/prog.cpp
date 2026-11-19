/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& deref(int* p) { return *p; }`; `int* q = &x; deref(q) = source(); sink(x)` writes what `*q` names, which is `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& deref(int* p) { return *p; }
int main() { int x = 0; int* q = &x; deref(q) = source(); sink(x); return 0; }
