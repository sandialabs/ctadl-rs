/* C++ reference parameters after #154, POSITIVE: `out += v` through `int& out`. A non-const scalar reference is a cell: its value is at `@p0.deref`, and the caller passes the argument through a temp and copies it back. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void add(int& out, int v) { out += v; }
int main() { int x = 0; add(x, source()); sink(x); return 0; }
