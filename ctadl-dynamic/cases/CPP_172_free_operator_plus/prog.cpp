/* C++ free operators (spec 032), POSITIVE (`f1_free_op_plus`). A free operator was never lowered (its name is no identifier) and `a + b` blended its operands. Now `a + b` calls `operator+(a, b)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; };
S operator+(const S& a, const S& b) { S r; r.v = source(); return r; }
int main() { S a; a.v = 0; S b; b.v = 0; S c = a + b; sink(c.v); return 0; }
