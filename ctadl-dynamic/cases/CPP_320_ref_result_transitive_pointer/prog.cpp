/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& deref(int* p) { return *p; }`; `int& outer(int* p) { return deref(p); }`; `outer(&x) = source(); sink(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& deref(int* p) { return *p; }
int& outer(int* p) { return deref(p); }
int main() { int x = 0; outer(&x) = source(); sink(x); return 0; }
