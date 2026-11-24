/* C++ reference locals bind their locations (spec 051), POSITIVE: `int a[2]; int& r = a[1]; r = source(); sink(a[1])`. An element initializer is aliased, not copied. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { int a[2] = {0, 0}; int& r = a[1]; r = source(); sink(a[1]); return 0; }
