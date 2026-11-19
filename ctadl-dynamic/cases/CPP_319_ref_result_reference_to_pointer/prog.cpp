/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& rp(int*& p) { return *p; }`; `int* q = &x; rp(q) = source(); sink(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& rp(int*& p) { return *p; }
int main() { int x = 0; int* q = &x; rp(q) = source(); sink(x); return 0; }
