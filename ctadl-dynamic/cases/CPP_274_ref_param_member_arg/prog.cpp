/* C++ reference parameters after #154, POSITIVE: a member `p.v` passed to `int& out` is copied back into `p.v` after the call. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int v; int w; };
void set_ref(int& out, int v) { out = v; }
int main() { P p; p.v = 0; p.w = 0; set_ref(p.v, source()); sink(p.v); return 0; }
