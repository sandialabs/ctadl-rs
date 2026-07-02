/* C++ static data member (spec 015, FR-1/FR-3), POSITIVE. `Counter::total` is a `static`
   data member: a single class-scoped GLOBAL, not a per-object field. A `static` setter writes
   it and a `static` getter reads it across two separate calls (there is no object). Before this
   slice CTADL modeled the methods as instance methods with an implicit `this`, so the setter's
   argument bound to the (unpassed) receiver and the flow was MISSED (`s=none d=flow`,
   soundness-disagree). Now a static data member is modeled as the global `Counter::total` and a
   static method has no `this`, so `Counter::add(source())` writes the global and
   `Counter::get()` reads it: the taint flows. DFSan observes the same global write/read:
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Counter {
    static int total;                              /* class-scoped global, not per-object */
    static void add(int x) { total = x; }          /* no `this`; writes the global */
    static int get() { return total; }             /* no `this`; reads the global */
};
int Counter::total = 0;                             /* out-of-line definition (storage) */

int main() {
    Counter::add(source());                         /* taints the static member */
    sink(Counter::get());                           /* reads it back -- flows */
    return 0;
}
