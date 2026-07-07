/* C++ scope-exit / RAII destructors (spec 017, FR-3), NEGATIVE — field sensitivity through the
   scope-exit destructor call. The setter taints member `v`; the destructor `~Widget(){ w = 0;
   sink(w); }` sinks a DISTINCT member `w` (set to 0). The destructor runs at the block's closing
   `}` (a scope-exit call), but `v` and `w` are separate field-named access paths, so the taint the
   setter wrote into `v` never crosses to `w` — the destructor sinks an untainted member. If the
   scope-exit destructor edge smeared taint across the whole object this would risk a spurious flow.
   CTADL matches DFSan: s=none d=none agree — field sensitivity survives the scope-exit destructor
   dispatch. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    int w;
    void set(int x) { v = x; }
    ~Widget() { w = 0; sink(w); }      /* sinks the DISTINCT member w (=0), not the tainted v */
};

int main() {
    { Widget wi; wi.set(source()); }   /* taints member v only; ~Widget sinks distinct w at `}` */
    return 0;
}
