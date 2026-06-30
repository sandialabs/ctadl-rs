/* C++ instance method (getter): the tainted value is written into the member `v` directly,
   then flows OUT through a value-returning member function `get()` that returns `this.v`.
   DFSan observes the flow through the method; CTADL models `Box::get(this: ByRef) -> this.v`. */
extern "C" int source();
extern "C" void sink(int);

class Box {
  public:
    int v;
    int get() { return v; }
};

int main() {
    Box b;
    b.v = source();
    sink(b.get());
    return 0;
}
