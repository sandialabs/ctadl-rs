/* C++ member-object receivers (spec 030), POSITIVE (`m2_member_method_from_main`), formerly
   F4: `o.in.set(source()); sink(o.in.get())`. Both calls dispatch on the member. The taint sits
   at `.in.v`, a path only the callees spell; since spec 050 the receiver's write-back stores each
   member by name, which spells it, so the path is admissible. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; In() : v(0) {} void set(int x) { v = x; } int get() { return v; } };
struct Out { In in; };
int main() { Out o; o.in.set(source()); sink(o.in.get()); return 0; }
