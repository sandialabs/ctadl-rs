/* C++ free operators (spec 032), NEGATIVE (`f8_scalars_untouched_negative`). `x + y` on ints must stay the built-in `+` even though a free `operator+` (returning source()) exists. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; };
int operator+(const S& a, const S& b) { return source(); }
int main() { int x = 1; int y = 2; sink(x + y); return 0; }
