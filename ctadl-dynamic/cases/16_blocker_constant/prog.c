/* BLOCKER (expect no flow): drop() ignores its tainted argument and returns a
   constant, so nothing tainted reaches the sink. Tests precision -- CTADL must not
   over-report a flow here. */
int source();
void sink(int);

int drop(int p) { return 0; }

int main() {
    int s = source();
    int r = drop(s);
    sink(r);
    return 0;
}
