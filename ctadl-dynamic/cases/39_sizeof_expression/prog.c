/* GAP: sizeof_expression. `sizeof(x)` is a compile-time size and does NOT evaluate x,
   so there is NO real flow -- but CTADL can't even parse `sizeof_expression` (expected
   ERR 78). DFSan: no flow (correct).  Expected harness verdict: frontend-error
   (the parse gap; once it lowers, this becomes a no-flow regression test). */
int source();
void sink(int);

int main() {
    int x = source();
    int y = sizeof(x);   /* sizeof_expression -- does not read x */
    sink(y);
    return 0;
}
