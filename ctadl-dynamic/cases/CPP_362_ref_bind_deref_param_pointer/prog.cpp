/* C++ reference locals bind their locations (spec 051), POSITIVE: `void f(int* p) { int& r = *p; r = source(); }`, then `f(&x); sink(x)`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

void f(int* p) { int& r = *p; r = source(); }
int main() { int x = 0; f(&x); sink(x); return 0; }
