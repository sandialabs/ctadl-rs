/* C++ qualified calls and constructor forms (spec 044), POSITIVE (`s07_base_call_out_of_line`). `Base::init(x)` in an out-of-line derived method. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; void init(int x) { v = x; } };
struct D : Base { void setup(int x); };
void D::setup(int x) { Base::init(x); }
int main() { D d; d.setup(source()); sink(d.v); return 0; }
