/* C++ name lookup (spec 037), POSITIVE (`n9_nested_class_out_of_line`). `Outer::Inner i;` and the out-of-line `int Outer::Inner::get()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Outer { struct Inner { int v; int get(); }; };
int Outer::Inner::get() { return v; }
int main() { Outer::Inner i; i.v = source(); sink(i.get()); return 0; }
