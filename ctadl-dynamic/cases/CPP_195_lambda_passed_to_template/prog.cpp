/* C++ lambdas (spec 034), POSITIVE (`l6_passed_to_template`). A lambda passed to `apply(F f, int x)` and called there as `f(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class F> int apply(F f, int x) { return f(x); }
int main() { sink(apply([](int v) { return v + 1; }, source())); return 0; }
