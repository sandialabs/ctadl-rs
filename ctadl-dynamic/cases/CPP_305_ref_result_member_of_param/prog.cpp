/* C++ reference results as locations (spec 048), POSITIVE: `int& fld(Q& q) { return q.v; }`; `fld(q) = source(); sink(q.v)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Q { int v; };
int& fld(Q& q) { return q.v; }
int main() { Q q; q.v = 0; fld(q) = source(); sink(q.v); return 0; }
