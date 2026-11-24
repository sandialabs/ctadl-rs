/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `void put(int& o)` passed as `void (*f)(int&)` to `apply`, which calls `f(x)`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

void put(int& o) { o = source(); }
void apply(void (*f)(int&), int& x) { f(x); }
int main() { int x = 0; apply(put, x); sink(x); return 0; }
