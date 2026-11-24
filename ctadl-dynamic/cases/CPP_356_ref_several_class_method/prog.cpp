/* C++ reference locals bind their locations (spec 051), POSITIVE: `Box& r = chooseb(a, b, 1); r.set(source()); sink(a.v)`. The receiver is copied in from both objects and written back into each, weakly, member by member. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct Box { int v; void set(int n) { v = n; } int get() { return v; } };
Box& chooseb(Box& a, Box& b, int c) { if (c) return a; return b; }
int main() { Box a{0}; Box b{0}; Box& r = chooseb(a, b, 1); r.set(source()); sink(a.v); return 0; }
