/* C++ reference locals bind their locations (spec 051), POSITIVE: `int* p = &x; int& r = *p; r = source(); sink(x)`. A dereference initializer is aliased. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { int x = 0; int* p = &x; int& r = *p; r = source(); sink(x); return 0; }
