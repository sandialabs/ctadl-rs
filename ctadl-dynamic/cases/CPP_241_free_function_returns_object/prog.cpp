/* C++ returned objects / function-pointer members (spec 041), POSITIVE (`q1_free_function_returns_object`). `make(x).get()` on a free function's by-value result. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int x; int get() const { return x; } };
P make(int v) { P p; p.x = v; return p; }
int main() { sink(make(source()).get()); return 0; }
