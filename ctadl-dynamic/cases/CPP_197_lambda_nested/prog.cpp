/* C++ lambdas (spec 034), POSITIVE (`l11_nested_lambda`). An inner lambda writes a variable two scopes out; every lambda in the chain names the slot of the body that declares it. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int y = 0; auto outer = [&]() { auto inner = [&]() { y = source(); }; inner(); }; outer(); sink(y); return 0; }
