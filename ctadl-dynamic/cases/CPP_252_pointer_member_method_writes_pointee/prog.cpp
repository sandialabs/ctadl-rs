/* C++ pointer members / new / namespaced bases (spec 042), POSITIVE (`k2_pointer_member_method`). `in->set(x)` dispatches, but `in` holds `&i` stored by the constructor, and a write through a pointer kept in a field does not reach the pointee: F7 (OPEN, known gap). s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; void set(int x) { v = x; } };
struct Out { In* in; Out(In* p) : in(p) {} void put(int x) { in->set(x); } };
int main() { In i; i.v = 0; Out o(&i); o.put(source()); sink(i.v); return 0; }
