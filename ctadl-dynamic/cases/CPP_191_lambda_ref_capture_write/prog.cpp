/* C++ lambdas (spec 034), POSITIVE (`l_c2_lambda_ref_capture_write`). A variable captured by reference lives in a global slot that the enclosing function and the lambda both name, so the write inside reaches the read outside. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int y = 0; auto g = [&]() { y = source(); }; g(); sink(y); return 0; }
