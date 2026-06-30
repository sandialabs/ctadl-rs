/* C++ out-of-line method definitions. The class `Box` only *declares* `set`/`get`; their
   bodies live at top level as `void Box::set(int){…}` / `int Box::get(){…}` (declarators are
   `qualified_identifier`s). CTADL discovers them and lowers each with the same implicit
   `this` (`ByRef`) as an inline method, so `b.set(source())` writes `this.v` and `b.get()`
   reads it back. DFSan observes the flow through the out-of-line bodies; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

class Box {
  public:
    int v;
    void set(int x);
    int get();
};

void Box::set(int x) { v = x; }
int Box::get() { return v; }

int main() {
    Box b;
    b.set(source());
    sink(b.get());
    return 0;
}
