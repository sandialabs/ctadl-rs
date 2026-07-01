/* C++ single inheritance — an inherited data member used inside a derived method. `store` is
   defined in `Derived` but writes `v`, a member of `Base`. CTADL flattens `Base`'s members
   into `Derived`, so the unqualified `v` inside `Derived::store` resolves to `this.v` (== the
   base subobject's field, shared with the derived object's field-named path). The inherited
   `Base::get` reads it back. DFSan observes source() -> d.v -> sink; CTADL matches
   (s=flow d=flow agree). */
extern "C" int source();
extern "C" void sink(int);

struct Base {
    int v;
    int get() { return v; }
};

struct Derived : Base {
    void store(int x) { v = x; }   /* writes the inherited member v */
};

int main() {
    Derived d;
    d.store(source());   /* derived method taints inherited d.v */
    sink(d.get());       /* inherited getter reads d.v back */
    return 0;
}
