/* C++ class-typed parameter as a method receiver (spec 020, FR-1/FR-2), POSITIVE — pointer.
   A pointer parameter's receiver is the pointer identifier itself (`p` in `p->set(...)`),
   exactly the convention spec 014 established for `Box* p = new Box()`: the pointer stands
   for the object it aliases. A pointer parameter is `ByRef`, so the member write through it
   reaches the caller's object, which `main` reads back with `a.get()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box { int v; void set(int x) { v = x; } int get() { return v; } };

void fill(Box* p) { p->set(source()); }   /* pointer parameter: the pointer is the receiver */

int main() {
    Box a;
    a.set(0);
    fill(&a);          /* taints a.v through the pointer parameter */
    sink(a.get());     /* reads the caller's object back -- flows */
    return 0;
}
