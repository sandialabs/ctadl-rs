/* C++ virtual destructors (spec 016, FR-3), POSITIVE — the BASE destructor in the chain sinks. A
   `Derived` is heap-allocated through a `Base*` and tainted, then `delete p`. Here it is the BASE
   destructor `virtual ~Base(){ sink(v); }` that moves the taint, while `~Derived(){}` is empty. At
   runtime `delete p` runs ~Derived then ~Base; ~Base sinks the tainted member -- a flow. CTADL's
   CHA target set for the virtual delete on the static type `Base` includes the static-type
   destructor `Base::~Base` (the base in the chain) alongside the subtree's `Derived::~Derived`, so
   the base destructor's sink is captured -- the C++ derived-then-base chain is soundly covered.
   DFSan runs both destructors: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    virtual ~Base() { sink(v); }       /* the BASE destructor moves the taint */
};

struct Derived : Base {
    ~Derived() {}                      /* empty override; the base dtor still runs in the chain */
};

int main() {
    Base* p = new Derived();
    p->set(source());                  /* taints the object's member v */
    delete p;                          /* runs ~Derived then ~Base -- ~Base sinks tainted v */
    return 0;
}
