/* GAP (soundness): union field overlap. `u.a` and `u.b` share storage, so writing u.a
   taints u.b. CTADL models a union like an ordinary field-sensitive struct (disjoint
   .a/.b), so the flow is dropped. DFSan: flow (same bytes).
   Expected harness verdict: soundness-disagree (static=none, dynamic=flow). */
int source();
void sink(int);
union U { int a; int b; };

int main() {
    union U u;
    u.a = source();
    sink(u.b);       /* aliases u.a */
    return 0;
}
