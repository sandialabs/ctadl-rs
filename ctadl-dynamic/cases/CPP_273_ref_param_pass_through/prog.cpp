/* C++ reference parameters after #154, POSITIVE: `b(int& o)` passes `o` on to `a(int& o)`, which writes it. The inner call copies back through `@p0.deref`, which reaches the outer caller. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void a(int& o) { o = source(); }
void b(int& o) { a(o); }
int main() { int x = 0; b(x); sink(x); return 0; }
