/* C++ member-object receivers (spec 030) x F4 (OPEN, known gap; `m2_member_method_from_main`):
   `o.in.set(source()); sink(o.in.get())`. Both calls dispatch on the member, but the taint must
   sit at `o.in.v`, a path only callees spell, so the admissibility gate drops it (C case 38).
   s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; In() : v(0) {} void set(int x) { v = x; } int get() { return v; } };
struct Out { In in; };
int main() { Out o; o.in.set(source()); sink(o.in.get()); return 0; }
