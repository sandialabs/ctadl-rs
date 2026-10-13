/* C++ lambdas (spec 034), POSITIVE (`l_c1_lambda_param`). A lambda used to be a frontend error. It is lowered as its own function; `f` holds a pointer to it, and `f(x)` is the indirect call the index resolves. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { auto f = [](int x) { sink(x); }; f(source()); return 0; }
