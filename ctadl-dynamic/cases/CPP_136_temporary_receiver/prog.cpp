/* C++ temporaries (spec 027), POSITIVE (`t3_temp_method_call`): a method called directly on a
   temporary, `B(source()).get()`. The receiver resolves to the constructed object.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int x) : v(x) {} int get() { return v; } };
int main() { sink(B(source()).get()); return 0; }
