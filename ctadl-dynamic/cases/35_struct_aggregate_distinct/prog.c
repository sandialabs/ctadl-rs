/* Struct aggregate initializer, field-sensitivity NEGATIVE: `struct P p = { 0, s };` puts the
   tainted element in the SECOND position, so it initializes `p.y`; the sink reads the distinct
   member `p.x` (initialized to a constant 0). This is what positional mapping buys -- an
   over-approximating lowering that assigned both elements to `p` itself would report a flow
   here. DFSan agrees no taint reaches the sink: `s=none d=none agree`.
   Companion positive: case 34. Spec 019. */
int source();
void sink(int);

struct P {
    int x;
    int y;
};

int main() {
    int s = source();
    struct P p = { 0, s };
    sink(p.x);
    return 0;
}
