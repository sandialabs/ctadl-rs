/* C++ lambdas (spec 034), POSITIVE (`l10_param_captured_by_ref`). A parameter captured by reference is copied into its slot at entry. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void fill(int seed) { auto f = [&]() { sink(seed); }; f(); }
int main() { fill(source()); return 0; }
