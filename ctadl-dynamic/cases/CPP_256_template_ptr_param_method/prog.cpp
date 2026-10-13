/* C++ template-dependent calls (spec 043), POSITIVE (`d4_template_ptr_param_method`). `t->take(x)` with `T* t`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Sink { void take(int x) { sink(x); } };
template <class T> void feed(T* t, int x) { t->take(x); }
int main() { Sink s; feed(&s, source()); return 0; }
