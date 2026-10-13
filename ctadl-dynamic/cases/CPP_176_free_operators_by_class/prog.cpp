/* C++ free operators (spec 032), POSITIVE (`f7_free_op_two_classes_positive`). Two same-arity free `operator+` (for A and for B) are two functions; `a1 + a2` reaches the A one. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct A { int v; }; struct B { int v; };
int operator+(const A& x, const A& y) { return x.v; }
int operator+(const B& x, const B& y) { return 0; }
int main() { A a1; A a2; a1.v = source(); a2.v = 0; sink(a1 + a2); return 0; }
