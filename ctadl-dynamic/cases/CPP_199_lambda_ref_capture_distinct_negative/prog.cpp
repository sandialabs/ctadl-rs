/* C++ lambdas (spec 034), NEGATIVE (`l13_ref_capture_distinct_negative`). Only `y` is written in the lambda; `w` stays clean. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int y = 0; int w = 0; auto g = [&]() { y = source(); }; g(); sink(w); return y - y; }
