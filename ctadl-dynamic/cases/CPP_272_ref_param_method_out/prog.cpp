/* C++ reference parameters after #154, POSITIVE: a method writes its `int& o` parameter; the method call passes `x` through a temp cell and copies it back. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int k; void fill(int& o) { o = source(); } };
int main() { S s; s.k = 0; int x = 0; s.fill(x); sink(x); return 0; }
