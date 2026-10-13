/* F5 (OPEN, known gap): `(p + 1)->v` and `arr[1].v` name the same member, but the C pointer
   model lowers them differently: `p->f` is `p.f`, while `arr[1].f` is `arr.[1].deref.f`. A
   write through one spelling is not seen through the other. d=flow, s=none. */
int source();
void sink(int);

struct V { int v; };

int main() {
    struct V arr[2];
    arr[0].v = 0;
    arr[1].v = source();
    struct V* p = arr;
    struct V* q = p + 1;
    sink(q->v);
    return 0;
}
