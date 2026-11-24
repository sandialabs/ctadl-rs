/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = argc > 0 ? x : y; r = source(); sink(x)`. A choice of locations binds both. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main(int argc, char**) { int x = 0; int y = 0; int& r = argc > 0 ? x : y; r = source(); sink(x); return 0; }
