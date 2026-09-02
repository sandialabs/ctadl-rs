/* C++ early-exit destructors (spec 021, FR-1/FR-3), POSITIVE, MULTIPLE FRAMES — the probed gap
   `p4f_dtor_early_return_nested`. The TAINTED object is the OUTER `w`; the `return 1` sits two
   scopes down (inside the `if` arm, inside a bare block that owns its own `Widget x`). C++
   destroys the objects of every scope the `return` leaves, innermost scope first: `x`, then `w`.
   Only `w` is tainted, so the flow depends on the OUTERMOST frame being unwound — the case spec
   017 could not reach at all (it emitted destructors only at a scope's fall-through `}`, and this
   path has none), reported s=none against DFSan's d=flow. Spec 021's `return` unwinds every open
   frame of the function, innermost first, so both calls are emitted in that order before the
   `Return` terminator and the outer object's `~Widget` moves the taint. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    Widget() : v(0) {}
    void set(int x) { v = x; }
    ~Widget() { sink(v); }
};

int use(int c) {
    Widget w;
    w.set(source());                   /* the TAINTED object lives in the function's own scope */
    {
        Widget x;                      /* an inner scope with its own (untainted) object */
        if (c) { return 1; }           /* destroys `x`, then `w` — two frames deep */
    }
    return 0;
}

int main() { return use(1); }
