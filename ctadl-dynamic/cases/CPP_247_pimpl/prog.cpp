/* C++ pointer members / new / namespaced bases (spec 042), POSITIVE (`m11_pimpl`). `p(new Impl())` in a member initializer used to be a frontend error (`new` only worked initializing a declaration). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Impl { int secret; };
struct Facade { Impl* p; Facade() : p(new Impl()) {} ~Facade() { delete p; } void store(int v) { p->secret = v; } int load() const { return p->secret; } };
int main() { Facade f; f.store(source()); sink(f.load()); return 0; }
