/* C++ operator overloading (spec 023, FR-4), POSITIVE — the write-through half of the subscript
   operator (`p2g_operator_subscript_write`). `a[0] = source()` with `int& operator[](int)` is two
   things: the operator runs, and the store lands on the location the REFERENCE it returned
   denotes. The frontend used to do neither — it stored into the built-in element path
   `a.[0].deref`, which `a.get()` never reads, so the flow was lost (s=none against DFSan's
   d=flow). Discovery now records that `Vec::operator[]` returns a reference to the member `v`
   (`ClassInfo::returns_member`), so the assignment emits the call and then stores into `a.v` —
   the storage that reference names, and where `get()` reads. An `operator[]` whose body returns
   anything but a bare member is NOT modelled: the assignment then keeps its built-in lowering
   rather than inventing a target. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Vec {
    int v;
    Vec() : v(0) {}
    int& operator[](int i) { return v; }   /* returns a reference to the member `v` */
    int get() { return v; }
};

int main() {
    Vec a;
    a[0] = source();       /* Vec::operator[](&a, 0), then a store into a.v */
    sink(a.get());
    return 0;
}
