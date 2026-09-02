/* C++ class-typed parameter as a method receiver (spec 020, FR-2), NEGATIVE at runtime —
   an EXPECTED PRECISION GAP for CTADL.

   C++ copies a by-value argument at the call, so `void fill(Box b)` writes its own copy and
   `main`'s `a` is untouched: DFSan observes no flow (d=none), and the by-value/by-reference
   contrast against CPP_100 is exactly what the parameter modes encode -- `Box b` is `ByVal`
   here, `Box&` is `ByRef` there.

   CTADL reports the flow anyway (s=flow), and NOT because of anything C++-specific: it is
   the value-copy/alias model the IR uses for every aggregate, in C just as much as in C++.
   An aggregate assignment `T c = b` has to be treated as *possibly aliasing* at every field,
   because the IR has no types and `b` may be a pointer -- so a later store to `c.f` is also a
   store to `b.f`. The same three-line C program shows it with no parameter and no class in
   sight:

       struct Box { int v; };
       int main(void) { struct Box a; struct Box c = a; c.v = source(); sink(a.v); }   // s=flow

   and it is what makes a by-value formal carry a *field* write back to its actual
   (`facts::isout`: `ByVal => !ap.is_empty()`). Blocking it in the frontend would mean copying
   the parameter member-wise instead, which is precise at depth 1 but *drops* taint held below
   a nested aggregate member (`c.i = a.i` loses `a.i.v` -- measured) -- trading a precision gap
   for a soundness gap, which the analysis contract forbids. So the over-approximation stands,
   and this case pins it: s=flow d=none, an expected precision gap, sound in CTADL's direction.

   What this case DOES regression-protect is that the write-back is the only difference: the
   spec-020 registration must not otherwise change what a by-value parameter means. */
extern "C" int source();
extern "C" void sink(int);

struct Box { int v; void set(int x) { v = x; } int get() { return v; } };

void fill(Box b) { b.set(source()); }   /* by-value: C++ writes the callee's own copy */

int main() {
    Box a;
    a.set(0);
    fill(a);           /* the caller's object is NOT written at runtime */
    sink(a.get());     /* reads the untainted a.v -- no runtime flow */
    return 0;
}
