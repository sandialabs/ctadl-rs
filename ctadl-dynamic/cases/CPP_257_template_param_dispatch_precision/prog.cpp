/* C++ template-dependent calls (spec 043), NEGATIVE (`d5_template_param_distinct_negative`). EXPECTED PRECISION GAP: the instance is `B` (whose `handle` stores 0), but a dependent call reaches every class declaring `handle`, `A`'s too. That is the declared price of soundness under type erasure. s=flow d=none. */
extern "C" int source();
extern "C" void sink(int);

struct A { int v; void handle(int x) { v = x; } };
struct B { int v; void handle(int x) { v = 0; } };
template <class T> void run(T& t, int x) { t.handle(x); }
int main() { B b; b.v = 0; run(b, source()); sink(b.v); return 0; }
