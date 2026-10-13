/* C++ temporaries (spec 027), POSITIVE (`t12_temp_dtor_runs`): a temporary's destructor runs.
   C++ runs it at the end of the full expression; CTADL runs it at the enclosing scope's exit,
   which is equivalent here because nothing can reach the temporary in between. s=flow d=flow
   agree. */
extern "C" int source();
extern "C" void sink(int);

struct G { int v; G(int x) : v(x) {} ~G() { sink(v); } };
int main() { int r = G(source()).v; return r - r; }
