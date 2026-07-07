/* C++ virtual destructors (spec 016, FR-3), NEGATIVE — field sensitivity through the destructor
   call. The setter taints member `v`; the destructor `~Derived(){ sink(w); }` sinks a DISTINCT
   member `w` (set to 0). The destructor runs at `delete p` (a virtual CHA call), but `v` and `w`
   are separate field-named access paths, so the taint `set` wrote into `v` never crosses to `w`
   -- the destructor sinks an untainted member. If the destructor edge smeared taint across the
   whole object this would risk a spurious flow. CTADL matches DFSan: s=none d=none agree -- field
   sensitivity survives the CHA destructor dispatch. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    int w;
    void set(int x) { v = x; }
    virtual ~Base() {}
};

struct Derived : Base {
    ~Derived() { sink(w); }            /* sinks the DISTINCT member w, not the tainted v */
};

int main() {
    Base* p = new Derived();
    p->w = 0;                          /* distinct member w holds a constant (no label) */
    p->set(source());                  /* taints member v only */
    delete p;                          /* ~Derived sinks w -- distinct, untainted */
    return 0;
}
