/* GAP (soundness): write-through-alias to a local's address. `p = &x; *p = src;` taints
   x, read back at the sink. CTADL handles READING through an alias (cf. corpus case 14)
   and *out=src through a pointer PARAM (case 15), but does not model a *local's* address
   being written through, so src never reaches x. DFSan: flow.
   Expected harness verdict: soundness-disagree (static=none, dynamic=flow). */
int source();
void sink(int);

int main() {
    int x = 0;
    int src = source();
    int *p = &x;
    *p = src;       /* write through the alias back into x */
    sink(x);
    return 0;
}
