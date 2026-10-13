/* C++ lambdas (spec 034), NEGATIVE (`l7_value_capture_snapshot_negative`). A by-value capture is a snapshot: `x` is tainted only after the closure copied it. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int x = 0; auto f = [x]() { return x; }; x = source(); sink(f()); return x - x; }
