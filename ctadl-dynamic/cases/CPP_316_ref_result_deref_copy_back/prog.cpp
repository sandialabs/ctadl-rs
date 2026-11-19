/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& deref(int* p) { return *p; }`; `set(deref(&x))` copies `set`'s reference cell back into `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& deref(int* p) { return *p; }
void set(int& o) { o = source(); }
int main() { int x = 0; set(deref(&x)); sink(x); return 0; }
