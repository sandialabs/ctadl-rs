/* C++ distinct static data members (spec 015, FR-1/FR-3), NEGATIVE — field sensitivity among
   statics. `Counter::a` and `Counter::b` are two DISTINCT `static` (class-scoped global) members.
   A static setter taints `a`; the sink reads `b`, which is only ever the constant 0. Because each
   static member is modeled as its own global (`Counter::a` vs `Counter::b`), the taint on `a`
   does not reach the read of `b` -- no false flow. DFSan agrees `b` is untainted:
   s=none d=none agree. (If the two statics were conflated into one location this would become
   s=flow, so this pins per-member field sensitivity.) */
extern "C" int source();
extern "C" void sink(int);

struct Counter {
    static int a;
    static int b;
    static void seta(int x) { a = x; }              /* taints a only */
    static int getb() { return b; }                 /* reads b (always 0) */
};
int Counter::a = 0;
int Counter::b = 0;

int main() {
    Counter::seta(source());                        /* taints the distinct member a */
    sink(Counter::getb());                          /* reads b -- no flow */
    return 0;
}
