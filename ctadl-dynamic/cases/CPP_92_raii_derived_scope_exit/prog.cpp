/* C++ scope-exit / RAII destructors (spec 017, FR-2/FR-3), POSITIVE — a DERIVED stack object
   destroys as its EXACT declared type. A stack `Derived d` (base declares an empty `~Base(){}`) with
   `~Derived(){ sink(v); }` is tainted (`d.set(source())`, an inherited setter writing member `v`).
   A stack object's dynamic type equals its declared type — it can never be a subclass — so scope
   exit is always the single-target exact-type destructor `Derived::~Derived` (no CHA, no virtual
   dispatch), which sinks the tainted member. At runtime `~Derived` runs at the block's closing `}`,
   so DFSan observes the flow; spec 017 emits exactly `DirectCall Derived::~Derived(d)` at the
   scope's fall-through exit. Before this slice a stack destructor never ran (s=none d=flow); now
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void set(int x) { v = x; }
    ~Base() {}                         /* empty base destructor; the exact-type dtor is ~Derived */
};

struct Derived : Base {
    ~Derived() { sink(v); }            /* the declared-type destructor — runs at scope exit */
};

int main() {
    { Derived d; d.set(source()); }    /* d destroyed as exactly Derived::~Derived at the `}` */
    return 0;
}
