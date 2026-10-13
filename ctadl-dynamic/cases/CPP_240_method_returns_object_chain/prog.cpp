/* C++ returned objects / function-pointer members (spec 041), POSITIVE (`p16_object_return_chain`). `p.twice().get()`: an object returned by value is a receiver of its class. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int x; P(int v) : x(v) {} P twice() const { return P(x + x); } int get() const { return x; } };
int main() { P p(source()); sink(p.twice().get()); return 0; }
