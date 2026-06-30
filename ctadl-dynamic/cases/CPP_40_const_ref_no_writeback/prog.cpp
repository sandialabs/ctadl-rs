/* Negative control for const references (no write-back). A `const T&` parameter is read-only,
   NOT a write-back out-param: passing the clean variable `x` by const-reference cannot taint
   it. The program does produce taint (`t = source()`), but that taint never reaches the sink,
   which reads the still-clean `x`. CTADL models `const T&` as `ByVal` (no `isout` write-back),
   so it must report no flow here (s=none); DFSan agrees (x's shadow stays clean, d=none). If
   `const T&` were wrongly modeled as a write-back `ByRef`, this would risk a spurious flow. */
extern "C" int source();
extern "C" void sink(int);

int read(const int& r) {
    return r;
}

int main() {
    int t = source();   /* real taint exists in the program */
    int x = 0;          /* clean */
    int y = read(x);    /* x passed by const& : read-only, cannot be written back */
    sink(x);            /* x is still clean -> no flow */
    return y - t;       /* keep t and y live so DFSan tracks them */
}
