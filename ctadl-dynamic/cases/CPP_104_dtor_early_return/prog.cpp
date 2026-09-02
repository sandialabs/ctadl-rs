/* C++ early-exit destructors (spec 021, FR-1/FR-3), POSITIVE — the probed soundness gap
   (`p4a_dtor_early_return`). A stack `Widget w` is tainted (`w.set(source())`) and its destructor
   `~Widget(){ sink(v); }` sinks the tainted member. `use` leaves through the `return 1` inside the
   `if` arm, NOT by falling off the end of its body, and C++ runs `~Widget` on that path too — so
   DFSan observes the flow. Spec 017 emitted a scope's destructors only at its normal fall-through
   exit (`walk_compound_body` called `scope_exit` only when the body did not diverge), so the
   `return 1` block ran no destructor at all and CTADL reported s=none against DFSan's d=flow.
   Spec 021 unwinds every open destructor frame of the function at a `return`, after the returned
   value is evaluated and before the terminator, so the early path emits `Widget::~Widget(w)` just
   like the fall-through one. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    Widget() : v(0) {}
    void set(int x) { v = x; }
    ~Widget() { sink(v); }             /* runs on EVERY exit from `use`, including the early one */
};

int use(int c) {
    Widget w;
    w.set(source());
    if (c) { return 1; }               /* leaves `use` here: `w` is destroyed on this path */
    return 0;
}

int main() { return use(1); }
