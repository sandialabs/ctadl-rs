/* C++ destruction chain (spec 030), NEGATIVE (`d4_dtor_chain_distinct_negative`): `~Base` sinks
   the untainted `u`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; int u; Base() : v(0), u(0) {} ~Base() { sink(u); } };
struct D : Base { ~D() {} };
int main() { { D d; d.v = source(); } return 0; }
