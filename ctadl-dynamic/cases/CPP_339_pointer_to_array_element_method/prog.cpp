/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `B arr[2]; B* p = &arr[1]; p->set(source()); sink(arr[1].v)`. `*p` is `arr.[1].deref`, the element `arr[1]` names; this was s=none before the merge too. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B arr[2]; B* p = &arr[1]; p->set(source()); sink(arr[1].v); return 0; }
