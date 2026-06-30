/* Frontend ingestion gap: a label on an EMPTY statement (`done: ;`). A `goto` whose
   target label has the null statement as its body fails ingestion -- the bare `;`
   reaches the expression flattener's catch-all (ERR 78). Labeling a *real* statement
   (e.g. `done: r = r;`) ingests fine, so this is specific to the empty-statement body.
   At runtime the `r = 0;` kill is jumped over, so DFSan observes the flow; the expected
   result once it parses is `flow`. Found by the broadened generator (M7).
   See ctadl-dynamic/KNOWN_FINDINGS.md. */
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
