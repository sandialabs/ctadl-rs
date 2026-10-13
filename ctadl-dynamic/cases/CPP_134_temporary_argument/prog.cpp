/* C++ temporaries (spec 027), POSITIVE (`t1_temp_arg_constref`). `B(source())` used to lower
   as a call to an undefined function `B`, dropping the argument. It now constructs a fresh
   object through `B::B` and passes that object. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int x) : v(x) {} };
int take(const B& b) { return b.v; }
int main() { sink(take(B(source()))); return 0; }
