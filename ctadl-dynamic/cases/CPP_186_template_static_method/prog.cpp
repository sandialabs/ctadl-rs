/* C++ templates (spec 033), POSITIVE (`t14_template_static_method`). `M<int>::twice(x)` calls the static member `M::twice`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct M { static T twice(T x) { return x + x; } };
int main() { sink(M<int>::twice(source())); return 0; }
