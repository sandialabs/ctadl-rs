/* C++ class-typed parameter as a method receiver (spec 020, FR-2), POSITIVE — by reference.
   A non-const `Box&` parameter is `ByRef` (spec 003's out-param model), so it aliases the
   caller's object: a member write through it must reach the caller. `fill(Box& b)` taints
   `b.v` via `Box::set`, whose implicit `this` is the by-ref arg 0, and the write flows back
   through `fill`'s own by-ref formal into `a`, which `main` then reads with `a.get()`.
   Before the parameter carried a class, `b.set(...)` was a free call to an empty stub and
   nothing was written at all. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box { int v; void set(int x) { v = x; } int get() { return v; } };

void fill(Box& b) { b.set(source()); }   /* reference parameter: writes through to the caller */

int main() {
    Box a;
    a.set(0);
    fill(a);           /* taints a.v through the reference parameter */
    sink(a.get());     /* reads the caller's object back -- flows */
    return 0;
}
