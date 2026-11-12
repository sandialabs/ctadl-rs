/* C++ reference parameters after #154, NEGATIVE at runtime, EXPECTED PRECISION GAP: `set_ref(x, 0)` overwrites a tainted `x`. The callee's store to the temp cell does not kill what the caller copied in, so CTADL keeps the old taint. Sound. s=flow d=none precision-disagree. */
extern "C" int source();
extern "C" void sink(int);

void set_ref(int& out, int v) { out = v; }
int main() { int x = source(); set_ref(x, 0); sink(x); return 0; }
