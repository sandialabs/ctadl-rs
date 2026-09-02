/* C++ early-exit destructors (spec 021, FR-2/FR-3), POSITIVE, `break` — the probed gap
   `p4h_dtor_break_last_stmt`. A tainted `Widget w` is constructed in a `for` body whose LAST
   statement is `break`: the body diverges, so it has no fall-through exit and spec 017's
   scope-exit emission (which only fires when a compound does not diverge) produced nothing —
   CTADL reported s=none while DFSan ran `~Widget` on the way out of the loop (d=flow). A `break`
   destroys the objects of every scope from the one it sits in up to and including the loop body,
   so spec 021 records the destructor-frame depth where the loop's `break_target` is set and emits
   those frames' destructors in the `break`'s own block, immediately before the jump. The sibling
   `break`-in-an-`if`-arm shape (`CPP_9x`-era probes) keeps agreeing, now through its own emission
   rather than the body's fall-through. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    Widget() : v(0) {}
    void set(int x) { v = x; }
    ~Widget() { sink(v); }
};

int main() {
    for (int i = 0; i < 2; i++) {
        Widget w;
        w.set(source());
        break;                         /* leaves the loop body: `w` is destroyed here */
    }
    return 0;
}
