/* GAP: cast_expression. A cast is value-preserving for taint, so src reaches the
   sink. CTADL's tree-sitter frontend has no `cast_expression` arm (expected ERR 78).
   DFSan: flow.  Expected harness verdict: frontend-error. */
int source();
void sink(int);

int main() {
    int x = source();
    long y = (long)x;   /* cast_expression */
    int z = (int)y;
    sink(z);
    return 0;
}
