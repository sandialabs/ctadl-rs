/* C++ scope-exit / RAII destructors (spec 017, FR-1/FR-3), POSITIVE — the probed soundness gap. A
   class-typed STACK (automatic) object `Widget w` is declared in a bare inner block and tainted
   (`w.set(source())`); its destructor `~Widget(){ sink(v); }` sinks the tainted member. At runtime
   the destructor runs AUTOMATICALLY at the block's closing `}` — no `delete` — so DFSan observes
   the flow. Spec 016 emitted destructors only at `delete` (the explicit case), so CTADL never ran
   `~Widget` for a stack object and reported s=none while DFSan observed the flow (s=none d=flow
   soundness-disagree). Spec 017 records each stack object per scope and, at the scope's normal
   fall-through exit, emits a single-target exact-type destructor `DirectCall Widget::~Widget(w)`
   (the object itself as the by-ref receiver, reusing 016's non-virtual dtor branch), capturing the
   flow. A stack object's dynamic type equals its declared type, so this is single-target (no CHA).
   DFSan runs exactly this: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    void set(int x) { v = x; }
    ~Widget() { sink(v); }             /* runs at the enclosing block's `}` — moves taint */
};

int main() {
    { Widget w; w.set(source()); }     /* w destroyed at the inner block's `}`; ~Widget sinks v */
    return 0;
}
