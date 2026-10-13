/* C++ template-dependent calls (spec 043), POSITIVE (`d1_crtp`). `static_cast<D*>(this)->impl(x)` (CRTP) used to dispatch nowhere: `D` is a template parameter, erased. The call reaches `impl` in every class declaring it. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class D> struct Base { int run(int x) { return static_cast<D*>(this)->impl(x); } };
struct Impl : Base<Impl> { int impl(int x) { return x + 1; } };
int main() { Impl i; sink(i.run(source())); return 0; }
