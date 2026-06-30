/* Frontend ingestion gap: an aggregate initializer list `{ ... }`. Here an array is
   brace-initialized with a tainted element (`int a[2] = { s, 0 };`); a struct
   aggregate initializer (`struct P p = { s, 0 };`) fails the same way. The expression
   flattener has no `initializer_list` arm, so CTADL fails to ingest it (ERR 78).
   DFSan observes the flow a[0] <- s, so the expected result once it parses is `flow`.
   Found by the broadened generator (M7). See ctadl-dynamic/KNOWN_FINDINGS.md. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int a[2] = { s, 0 };
    sink(a[0]);
    return 0;
}
