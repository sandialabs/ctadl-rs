/* C++ member-object receivers (spec 030), POSITIVE (`m3_member_method_this_arrow`): `this->in.set(x)`
   dispatches to `In::set` on the member, copy-in/copy-out. Before, a field receiver did not
   resolve at all and the call was dropped. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; In() : v(0) {} void set(int x) { v = x; } };
struct Out { In in; void put(int x) { this->in.set(x); } };
int main() { Out o; o.put(source()); sink(o.in.v); return 0; }
