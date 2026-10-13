/* C++ free operators (spec 032), NEGATIVE (`f6_free_op_two_classes_negative`). The precision guard for shape resolution: `b1 + b2` must reach only the B operator (returns 0). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct A { int v; }; struct B { int v; };
int operator+(const A& x, const A& y) { return x.v; }
int operator+(const B& x, const B& y) { return 0; }
int main() { A a; a.v = source(); B b1; B b2; b1.v = 0; b2.v = 0; sink(b1 + b2); return a.v - a.v; }
