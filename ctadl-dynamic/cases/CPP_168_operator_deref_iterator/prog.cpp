/* C++ member operators (spec 031), POSITIVE (`o10_op_deref_iterator`). `*it` on an object is `it.operator*()`, not a dereference. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct It { int* p; int operator*() { return *p; } };
int main() { int x = source(); It it; it.p = &x; sink(*it); return 0; }
