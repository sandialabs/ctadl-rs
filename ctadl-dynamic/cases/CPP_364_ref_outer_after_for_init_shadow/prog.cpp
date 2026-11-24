/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = x; for (int r = 0; ..) { .. } r = source(); sink(x)`. The for initializer's scope ends with the loop, so `r` is the reference again. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main(int argc, char**) { int x = 0; int& r = x; for (int r = 0; r < argc; r++) { sink(r); } r = source(); sink(x); return 0; }
