/* C++ template-dependent calls (spec 043), POSITIVE (`d2_template_param_method`). `t.handle(x)` with `T& t` in a function template. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct H { int v; void handle(int x) { v = x; } };
template <class T> void run(T& t, int x) { t.handle(x); }
int main() { H h; h.v = 0; run(h, source()); sink(h.v); return 0; }
