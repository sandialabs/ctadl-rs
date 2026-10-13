/* C++ member-object receivers (spec 030), NEGATIVE (`m6_member_method_distinct_negative`): `set`
   taints `in.v`; `getu()` reads `in.u`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; int u; In() : v(0), u(0) {} void set(int x) { v = x; } int getu() { return u; } };
struct Out { In in; };
int main() { Out o; o.in.set(source()); sink(o.in.getu()); return 0; }
