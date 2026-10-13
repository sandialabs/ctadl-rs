/* C++ templates (spec 033), POSITIVE (`t3_fn_template_explicit`). `twice<int>(x)` lowered as an indirect call through a temp; it is the direct call `twice(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <typename T> T twice(T x) { return x + x; }
int main() { sink(twice<int>(source())); return 0; }
