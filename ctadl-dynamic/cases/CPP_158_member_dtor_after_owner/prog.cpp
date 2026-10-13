/* C++ destruction chain (spec 030), POSITIVE (`d3_member_dtor_with_own_dtor`): destroying an `Out`
   runs `~Out` and then its member object's `~In`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; In() : v(0) {} ~In() { sink(v); } };
struct Out { In in; int w; Out() : w(0) {} ~Out() { w = 1; } };
int main() { { Out o; o.in.v = source(); } return 0; }
