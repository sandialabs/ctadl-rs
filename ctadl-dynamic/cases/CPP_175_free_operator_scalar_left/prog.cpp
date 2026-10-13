/* C++ free operators (spec 032), POSITIVE (`f4_free_op_scalar_left`). The object is the *right* operand (`2 * s`); a free operator still applies. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; };
int operator*(int k, const S& s) { return k * s.v; }
int main() { S s; s.v = source(); sink(2 * s); return 0; }
