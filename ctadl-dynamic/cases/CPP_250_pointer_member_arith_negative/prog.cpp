/* C++ pointer members / new / namespaced bases (spec 042), NEGATIVE (`k4_pointer_member_arith_negative`). `p + k` on a pointer member stays pointer arithmetic even though the class declares `operator+` (returning source()). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct V { int v; int operator+(int x) { return source(); } };
struct H { V* p; int idx(int k) { V* q = p + k; return q->v; } };
int main() { V arr[2]; arr[0].v = 0; arr[1].v = 0; H h; h.p = arr; sink(h.idx(1)); return 0; }
