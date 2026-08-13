/* C++ mirror of case 35. Struct aggregate initializer, field-sensitivity NEGATIVE: `P p = { 0, s };`
   puts the tainted element in the SECOND position, so it initializes `p.y`; the sink reads the
   distinct member `p.x` (a constant 0). An over-approximating lowering that assigned both elements
   to `p` itself would report a flow here, so this pins the positional mapping. DFSan agrees no
   taint reaches the sink: `s=none d=none agree`. Companion positive: CPP_95. Spec 019. */
extern "C" int source();
extern "C" void sink(int);

struct P {
    int x;
    int y;
};

int main() {
    int s = source();
    P p = { 0, s };
    sink(p.x);
    return 0;
}
