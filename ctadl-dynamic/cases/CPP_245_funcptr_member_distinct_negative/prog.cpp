/* C++ returned objects / function-pointer members (spec 041), NEGATIVE (`q7_funcptr_member_distinct_negative`). `click` calls `other` (quiet), not `cb` (handler). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Btn { void (*cb)(int); void (*other)(int); void click(int x) { other(x); } };
void handler(int x) { sink(x); } void quiet(int x) { sink(0); }
int main() { Btn b; b.cb = handler; b.other = quiet; b.click(source()); return 0; }
