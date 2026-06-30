/* Field-sensitivity negative: source() taints member `a` (via a setter), but the sink reads
   a *different* member `b` through a getter. No flow exists at runtime (DFSan: d=none), and
   CTADL's field-sensitive member modeling must not spuriously connect a -> b (s=none). */
extern "C" int source();
extern "C" void sink(int);

struct Pair {
    int a;
    int b;
    void set_a(int x) { a = x; }
    int get_b() { return b; }
};

int main() {
    Pair p;
    p.set_a(source());
    sink(p.get_b());
    return 0;
}
