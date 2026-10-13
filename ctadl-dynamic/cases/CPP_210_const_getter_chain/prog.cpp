/* C++ reference results (spec 036), POSITIVE (`r2_const_getter_chain`). `o.inner().get()`: `inner` returns the member object `in` by reference, which becomes the receiver of `get`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; int get() const { return v; } }; struct Out { In in; const In& inner() const { return in; } };
int main() { Out o; o.in.v = source(); sink(o.inner().get()); return 0; }
