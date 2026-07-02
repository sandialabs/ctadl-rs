/* C++ static data member shared across a non-static and a static method (spec 015, FR-3),
   POSITIVE. `Counter::total` is a `static` (class-scoped global) member. A NON-static method
   `bump` (called on an object `c`, so it has an implicit `this`) writes the static member, and a
   `static` getter `get` (no `this`) reads it. Because a static member is one shared global
   regardless of how it is accessed, both the instance method's write and the static method's
   read bind to the same global `Counter::total` -- the taint written through the object's method
   flows out through the receiver-less static getter. DFSan observes the same shared location:
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Counter {
    static int total;                               /* class-scoped global */
    void bump(int x) { total = x; }                 /* NON-static: has `this`, writes the global */
    static int get() { return total; }              /* static: no `this`, reads the global */
};
int Counter::total = 0;

int main() {
    Counter c;
    c.bump(source());                               /* instance method writes the static member */
    sink(Counter::get());                           /* static getter reads it -- flows */
    return 0;
}
