/* C++ pointer members / new / namespaced bases (spec 042), POSITIVE (`k5_member_array_of_objects_method`). `items[0].set(x)` on a member array of objects. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; void set(int x) { v = x; } };
struct Bag { B items[2]; void fill(int x) { items[0].set(x); } };
int main() { Bag g; g.fill(source()); sink(g.items[0].v); return 0; }
