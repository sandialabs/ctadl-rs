/* Taint assigned only in a case that is NOT taken (switch(1) -> default, not case 2).
   At runtime x stays 0, so DFSan sees no flow. A path-insensitive static analysis
   treats every arm as reachable and may report the dead case -> precision-gap
   (expected imprecision, not a soundness bug). Mirrors 20_if_not_taken. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    switch (1) {
        case 2:
            x = s;
            break;
        default:
            x = 0;
            break;
    }
    sink(x);
    return 0;
}
