/* C++ free operators (spec 032), POSITIVE (`r1_op_arrow_reads`). `w->v` with `P::operator->` is `(w.operator->())->v`, which is now emitted. The flow still needs `w.p.v` with `.p` read in the callee, so it is F4 (OPEN, known gap), like any getter returning a member pointer. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; };
struct P { In* p; In* operator->() { return p; } };
int main() { In i; i.v = source(); P w; w.p = &i; sink(w->v); return 0; }
