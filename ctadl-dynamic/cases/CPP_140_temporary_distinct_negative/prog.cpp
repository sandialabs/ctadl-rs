/* C++ temporaries (spec 027), NEGATIVE (`t5_temp_distinct_member` + `t14_temp_dtor_distinct`):
   the constructor taints `v` while `getw()` reads the constant `w`. The precision guard: it
   fails if the temporary's members blend. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int w; B(int x) : v(x), w(0) {} int getw() { return w; } };
int main() { sink(B(source()).getw()); return 0; }
