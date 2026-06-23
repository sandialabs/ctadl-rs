/* Taint is conditionally KILLED on the (always-taken) path: s = source(); if(1) s=0.
   At runtime the sink gets 0, so DFSan sees no flow. A path-insensitive analysis may
   keep the source value alive across the merge -> precision-gap (expected). */
int source();
void sink(int);

int main() {
    int s = source();
    if (1) {
        s = 0;
    }
    sink(s);
    return 0;
}
