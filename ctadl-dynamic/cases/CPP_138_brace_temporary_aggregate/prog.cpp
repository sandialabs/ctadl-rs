/* C++ temporaries (spec 027), POSITIVE (`t13_ctorless_aggregate_temp`): `P{source(), 0}` on a
   constructor-less class is aggregate initialization. C++ names the compound literal's type bare
   (a `type_identifier`), which the record-layout lookup now accepts, so the members map by
   name. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int a; int b; };
int pick(P p) { return p.a; }
int main() { sink(pick(P{source(), 0})); return 0; }
