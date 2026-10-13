/* C++ lambdas (spec 034), POSITIVE (`l5_immediately_invoked`). `[&]() { … }()` calls through the closure, held in a temp. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int y = 0; [&]() { y = source(); }(); sink(y); return 0; }
