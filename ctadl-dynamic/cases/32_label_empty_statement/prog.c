/* A label on an EMPTY statement (`done: ;`), the body of a `goto` target. This was the
   `labeled_empty_statement` frontend ingestion gap: the bare `;` reached the expression
   flattener's catch-all (ERR 78) and failed the whole program. The null statement now
   lowers to a no-op, so the case runs as a plain regression test. At runtime the
   `r = 0;` kill is jumped over, so DFSan observes the flow, and CTADL agrees.
   Found by the broadened generator (M7). See ctadl-dynamic/KNOWN_FINDINGS.md. */
int source();
void sink(int);

int main() {
    int s = source();
    int r = s;
    goto done;
    r = 0;
done:
    ;
    sink(r);
    return 0;
}
