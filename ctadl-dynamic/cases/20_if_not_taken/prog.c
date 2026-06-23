/* Conditional, taint branch NEVER taken (if(0)). At runtime x stays 0, so DFSan
   sees no flow. A path-insensitive static analysis may still report a flow from the
   dead branch -> precision-gap (expected imprecision, not a soundness bug). */
int source();
void sink(int);

int main() {
    int s = source();
    int x = 0;
    if (0) {
        x = s;
    }
    sink(x);
    return 0;
}
