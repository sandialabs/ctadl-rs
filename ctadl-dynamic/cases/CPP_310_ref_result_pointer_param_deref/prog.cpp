/* C++ reference results as locations (spec 048), POSITIVE: `int& deref(int* p) { return *p; }`; `deref(&x) = source(); sink(x)` writes what `*(&x)` names (spec 049; was known gap F9). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& deref(int* p) { return *p; }
int main() { int x = 0; deref(&x) = source(); sink(x); return 0; }
