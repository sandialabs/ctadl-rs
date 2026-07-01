/* C++ single inheritance — field sensitivity through inheritance, NEGATIVE. The inherited
   setter `Base::setv` taints the *inherited* member `v`; the sink reads a *distinct derived*
   member `w` (set to a constant 0). `d.v` and `d.w` are separate field-named access paths, so
   the taint in `d.v` never crosses to `d.w` despite real taint entering the program. DFSan
   agrees: w's shadow stays clean (d=none). If inheritance smeared taint across the whole
   object, this would risk a spurious flow. CTADL matches DFSan: s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    void setv(int x) { v = x; }
};

struct Derived : Base {
    int w;
    int getw() { return w; }
};

int main() {
    Derived d;
    d.w = 0;             /* a distinct derived member, untainted */
    d.setv(source());    /* taints the inherited member d.v -- real taint enters */
    sink(d.getw());      /* reads d.w -- distinct, untainted */
    return 0;
}
