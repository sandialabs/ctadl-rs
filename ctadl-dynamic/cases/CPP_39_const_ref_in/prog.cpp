/* C++ const lvalue reference (inbound): `read(const int& r)` reads the referent and returns
   it. A `const T&` is read-only — the referent's value flows IN (reads of `r` are tainted iff
   the argument is) but nothing flows back. CTADL models `const T&` as `ByVal` (inbound only),
   so the tainted argument flows out through the return into the sink. DFSan sees the tainted
   value read through the const reference and returned. */
extern "C" int source();
extern "C" void sink(int);

int read(const int& r) {
    return r;
}

int main() {
    int x = source();
    sink(read(x));
    return 0;
}
