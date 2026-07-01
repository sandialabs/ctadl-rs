/* GAP (soundness): a non-constant subscript may alias a constant one. `a[n]` with n==0
   at runtime writes a[0], read back at the sink. CTADL lowers `a[n]` to a distinct
   `[_elem_]` field symbol and `a[0]` to `[0]`, treats them as disjoint, and drops the
   flow. DFSan: flow (n==0). `n` comes from a volatile so it isn't constant-folded.
   Expected harness verdict: soundness-disagree (static=none, dynamic=flow). */
int source();
void sink(int);

int main() {
    int a[4];
    volatile int vn = 0;
    int n = vn;          /* runtime 0, not constant-folded */
    int src = source();
    a[n] = src;          /* non-constant subscript -> [_elem_] */
    sink(a[0]);          /* constant subscript [0]; aliases a[n] when n==0 */
    return 0;
}
