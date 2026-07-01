/* GAP: conditional_expression (ternary). `c ? x : 0` yields x when c is truthy, so
   src reaches the sink. CTADL has no `conditional_expression` arm (expected ERR 78).
   DFSan: flow (c==1).  Expected harness verdict: frontend-error. */
int source();
void sink(int);

int main() {
    int x = source();
    int c = 1;
    int y = c ? x : 0;   /* conditional_expression */
    sink(y);
    return 0;
}
