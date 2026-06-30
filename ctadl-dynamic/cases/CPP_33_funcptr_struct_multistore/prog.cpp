/* F2 regression (struct form): TWO function-pointer stores into the same struct
   (o.a = id; o.b = id), then an indirect call through o.a. The second store creates a
   new SSA version of `o`, and the stored target must propagate across it to the call
   site. This was dropped before the F2 fix (taint lost); see KNOWN_FINDINGS.md (F2).
   Parallels case 09 (single struct-field store, which always worked). */
extern "C" int source();
extern "C" void sink(int);

int id(int p) { return p; }

struct Ops {
    int (*a)(int);
    int (*b)(int);
};

int main() {
    struct Ops o;
    o.a = id;
    o.b = id;
    int s = source();
    int r = o.a(s);
    sink(r);
    return 0;
}
