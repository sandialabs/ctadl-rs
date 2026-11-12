/* C++ reference parameters after #154, POSITIVE, KNOWN GAP F8: a lambda's `int& o` keeps the root model, because a closure is called through a pointer that cannot pass a temp cell, and since #154 rebinding a parameter never reaches the caller. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { auto f = [](int& o) { o = source(); }; int x = 0; f(x); sink(x); return 0; }
