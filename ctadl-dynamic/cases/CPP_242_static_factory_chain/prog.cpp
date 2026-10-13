/* C++ returned objects / function-pointer members (spec 041), POSITIVE (`q2_static_factory_chain`). `W::make(x).get()` on a static factory's result. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct W { int v; static W make(int x) { W w; w.v = x; return w; } int get() { return v; } };
int main() { sink(W::make(source()).get()); return 0; }
