/* C++ lambdas (spec 034), POSITIVE (`l_c4_lambda_returns`). A lambda returning a value has return arity 1. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { auto f = [](int x) { return x + 1; }; sink(f(source())); return 0; }
