/* C++ subobject construction (spec 029), POSITIVE (`s5_member_object_explicit`): `: in(x)` runs
   `In::In` on the member. The receiver is the value loaded from `this.in` and is stored back
   afterwards (copy-in/copy-out), because index summaries do not write a callee's stores back
   through a load. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; In(int x) : v(x) {} };
struct Out { In in; int w; Out(int x) : in(x), w(0) {} };
int main() { Out o(source()); sink(o.in.v); return 0; }
