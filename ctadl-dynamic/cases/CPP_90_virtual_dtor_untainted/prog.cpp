/* C++ virtual destructors (spec 016, FR-3), NEGATIVE — the destructor sinks a member that was
   never tainted. A source exists in the program (`tainted`), but it never reaches the object: the
   member `v` is set to a constant 0, and the destructor `~Derived(){ sink(v); }` sinks that
   untainted `v` at `delete p`. The destructor edge must not invent taint the object never carried.
   CTADL matches DFSan: the source→sink path does not exist, so s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    virtual ~Base() {}
};

struct Derived : Base {
    ~Derived() { sink(v); }            /* sinks v on destruction */
};

int main() {
    int tainted = source();            /* real taint enters the program... */
    Base* p = new Derived();
    p->set(0);                         /* ...but the object's member v is a constant (untainted) */
    delete p;                          /* ~Derived sinks the untainted v -- no flow */
    return tainted - tainted;          /* keep `tainted` live; returning to main's caller is not a sink */
}
