/* C++ lambdas (spec 034), POSITIVE (`l_c3_lambda_value_capture_read`). A by-value capture is copied into its slot when the closure is created. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int y = source(); auto g = [y]() { sink(y); }; g(); return 0; }
