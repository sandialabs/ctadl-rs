/* C++ class-typed parameter as a method receiver (spec 020, FR-1), POSITIVE — by value.
   Every receiver the frontend could dispatch on used to be a *local*: only `walk_declaration`
   (and the construction/`new` paths) recorded a variable's class in `local_types`, so inside
   `void take(Box b)` the receiver `b` had no class, `dispatch_call` returned `None`, and
   `b.get()` lowered as a free call `get(b)` to an empty extern stub — the taint died there.
   With a class-typed parameter registered where the parameter is bound, `b.get()` is a
   `DirectCall Box::get(@p0)` with the parameter's own access path as the by-ref receiver, so
   the tainted member the caller wrote flows out of the getter and into the sink.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box { int v; void set(int x) { v = x; } int get() { return v; } };

void take(Box b) { sink(b.get()); }   /* by-value class parameter is the receiver: Box::get(@p0) */

int main() {
    Box a;
    a.set(source());   /* taints member a.v */
    take(a);           /* the by-value copy carries the tainted member in -- flows */
    return 0;
}
