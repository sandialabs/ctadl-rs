/* C++ overloading (by arity), instance methods — POSITIVE (arity 1).
   `Box::f` is overloaded: `f(int)` returns its one argument, `f(int,int)` returns its second.
   The call `b.f(source())` has ONE explicit argument, so it must dispatch to the arity-1 method
   overload (`Box::f#1`), which returns its argument — so source's taint reaches the sink. This
   pins that overloaded *methods* split by arity and a 1-arg call reaches the 1-arg method. DFSan
   runs exactly the arity-1 `Box::f` (returns its arg), so it sees the flow (`s=flow d=flow`). */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int f(int a) { return a; }
    int f(int a, int b) { return b; }
};

int main() {
    Box b;
    sink(b.f(source()));
    return 0;
}
