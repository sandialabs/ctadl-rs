/* C++ reference results (spec 036), POSITIVE (`r6_getter_member_object_set`). `o.inner().set(x)` dispatches on the member object and writes it back. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; void set(int x) { v = x; } }; struct Out { In in; In& inner() { return in; } };
int main() { Out o; o.inner().set(source()); sink(o.in.v); return 0; }
