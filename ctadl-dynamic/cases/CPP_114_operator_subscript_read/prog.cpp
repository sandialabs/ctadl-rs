/* C++ operator overloading (spec 023, FR-3), POSITIVE — the probed soundness gap
   (`p2b_operator_subscript`). `a[0]` on an object whose class declares `operator[]` is
   `a.operator[](0)`, not a read of the synthetic element path `a.[0].deref`. Reading that element
   path is why the case was s=none: the taint is in the member `v` (written by `set`), and a read
   at a path nothing ever wrote observes nothing — no base-path fallback would be sound. Now the
   subscript dispatches to `Vec::operator[]` with `a` as the by-ref receiver, so the body's
   `return v + i` reads the tainted member and the result flows to the sink. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Vec {
    int v;
    Vec() : v(0) {}
    void set(int x) { v = x; }
    int operator[](int i) { return v + i; }   /* returns the member, by value */
};

int main() {
    Vec a;
    a.set(source());
    sink(a[0]);            /* Vec::operator[](&a, 0) — not a read of a.[0].deref */
    return 0;
}
