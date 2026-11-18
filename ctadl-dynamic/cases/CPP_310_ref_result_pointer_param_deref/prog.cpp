/* C++ reference results as locations (spec 048), POSITIVE, KNOWN GAP F9: `int& deref(int* p) { return *p; }`; `deref(&x) = source(); sink(x)`. A place through a pointer parameter is out of spec 048's scope. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& deref(int* p) { return *p; }
int main() { int x = 0; deref(&x) = source(); sink(x); return 0; }
