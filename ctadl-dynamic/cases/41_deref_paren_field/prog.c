/* GAP: `(*p).x` parenthesized-deref-then-field. Should be identical to `p->x`, so the
   tainted field is read back at the sink. CTADL panics on this node shape in mod.rs
   (expects only field_expression). DFSan: flow.  Expected harness verdict:
   frontend-error (the ingestion panic is caught by the runner's catch_unwind). */
int source();
void sink(int);
struct S { int x; };

int main() {
    struct S s;
    struct S *p = &s;
    p->x = source();
    int y = (*p).x;   /* (*p).x -- parenthesized deref then field */
    sink(y);
    return 0;
}
