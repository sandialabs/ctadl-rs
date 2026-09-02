/* C++ early-exit destructors (spec 021, FR-3), NEGATIVE — the precision control
   (`p4g_dtor_early_return_distinct`). Identical to CPP_104's early-`return` shape, except that the
   destructor sinks a DIFFERENT member: `w.set(source())` taints `v`, while `~Widget(){ sink(u); }`
   reads `u`. The destructor really does run on the `return 1` path, and spec 021 really does emit
   the `Widget::~Widget(w)` call there — but the receiver is passed as an access path, so the
   destructor's `this.u` read never reaches the tainted `this.v`. This pins that the new unwinding
   adds no field-insensitive over-approximation: emitting a destructor on the early path must not
   invent a flow. DFSan agrees nothing tainted reaches the sink. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    int u;
    Widget() : v(0), u(0) {}
    void set(int x) { v = x; }         /* taints `v` ... */
    ~Widget() { sink(u); }             /* ... but the destructor sinks `u` — a different member */
};

int use(int c) {
    Widget w;
    w.set(source());
    if (c) { return 1; }
    return 0;
}

int main() { return use(1); }
