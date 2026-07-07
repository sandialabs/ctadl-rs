/* C++ virtual destructors (spec 016, FR-3), POSITIVE — the probed soundness gap. A `Derived` is
   heap-allocated through a `Base*` and tainted (`p->set(source())`), then `delete p`. The
   destructor is `virtual`, so `delete p` runs the DYNAMIC type's destructor chain — `~Derived()`
   then `~Base()` — and `~Derived(){ sink(v); }` sinks the tainted member. Before spec 016 `delete`
   was a taint no-op (spec 014), so CTADL never ran `~Derived` and reported s=none while DFSan
   observed the flow (s=none d=flow soundness-disagree). Spec 016 models `delete p` on a static
   type with a virtual destructor as a CHA multi-target call over the subtree destructors
   (`Base::~Base`, `Derived::~Derived`) with the referent as the by-ref receiver, capturing the
   flow through `Derived::~Derived`. DFSan runs exactly that chain: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    virtual ~Base() {}                 /* virtual destructor: delete runs the dynamic chain */
};

struct Derived : Base {
    ~Derived() { sink(v); }            /* moves taint at destruction time */
};

int main() {
    Base* p = new Derived();           /* static type Base, dynamic type Derived, on the heap */
    p->set(source());                  /* taints the object's member v */
    delete p;                          /* virtual: runs ~Derived -> sinks tainted v -- flows */
    return 0;
}
