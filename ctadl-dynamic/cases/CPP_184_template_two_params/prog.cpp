/* C++ templates (spec 033), POSITIVE (`t6_template_two_params`). `Pair<int, int> p(0, s)` -- this used to be a frontend error (the argument list reached the value lowering). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class K, class V> struct Pair { K k; V v; Pair(K a, V b) : k(a), v(b) {} V second() { return v; } };
int main() { Pair<int, int> p(0, source()); sink(p.second()); return 0; }
