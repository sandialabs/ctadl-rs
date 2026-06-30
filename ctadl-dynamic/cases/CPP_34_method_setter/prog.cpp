/* C++ instance method (setter): the tainted value flows IN through a member function
   `set(int)` that writes `this.v`, and the caller reads the member `v` directly. DFSan
   must observe the flow *through* the method; CTADL models the method as
   `Box::set(this: ByRef, x)` so the write to `this.v` propagates back to `b.v`. */
extern "C" int source();
extern "C" void sink(int);

class Box {
  public:
    int v;
    void set(int x) { v = x; }
};

int main() {
    Box b;
    b.set(source());
    sink(b.v);
    return 0;
}
