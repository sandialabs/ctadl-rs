/* C++ returned objects / function-pointer members (spec 041), POSITIVE (`p09_std_function_like_template`). A function passed to a templated holder's constructor (`Holder<…> h(id)`, a most-vexing parse) used to be read as a global variable; it is a function pointer. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class F> struct Holder { F f; Holder(F g) : f(g) {} int call(int x) { return f(x); } };
int id(int x) { return x; }
int main() { Holder<int (*)(int)> h(id); sink(h.call(source())); return 0; }
