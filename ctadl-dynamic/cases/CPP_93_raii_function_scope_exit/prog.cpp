/* C++ scope-exit / RAII destructors (spec 017, FR-2/FR-3), POSITIVE — a FUNCTION-body scope. The
   function body is itself a scope: a stack `Widget w` local is destroyed at the function's closing
   `}` (no explicit inner block, no `delete`). `use(int x)` taints the local (`w.set(x)`, x =
   source()), and `~Widget(){ sink(v); }` sinks the tainted member when `w` is destroyed at `use`'s
   `}`. At runtime DFSan observes the flow; spec 017 emits `DirectCall Widget::~Widget(w)` at the
   function body's normal fall-through exit (a void function, so it falls through — the divergence
   early-exit case is out of this slice's scope). Before this slice a stack destructor never ran
   (s=none d=flow); now s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Widget {
    int v;
    void set(int x) { v = x; }
    ~Widget() { sink(v); }             /* runs at use()'s closing `}` — moves taint */
};

void use(int x) {
    Widget w;                          /* a function-body-scope stack object */
    w.set(x);                          /* taints w.v */
}                                      /* w destroyed here — ~Widget sinks the tainted v */

int main() {
    use(source());                     /* passes the tainted value into use() */
    return 0;
}
