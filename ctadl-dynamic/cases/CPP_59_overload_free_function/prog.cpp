/* C++ overloading (by arity), free functions — POSITIVE (arity 1).
   `id` is overloaded: `id(int)` returns its one argument, `id(int,int)` returns its second.
   The call `id(source())` has ONE explicit argument, so it must resolve to the arity-1 overload
   (`id#1`), which returns its argument — so source's taint reaches the sink. This pins that an
   overloaded name is split by arity (the two definitions do not merge/clobber) and a 1-arg call
   reaches the 1-arg overload. DFSan runs exactly the arity-1 `id` (returns its arg), so it sees
   the flow; CTADL must match (`s=flow d=flow`). */
extern "C" int source();
extern "C" void sink(int);

int id(int a) { return a; }
int id(int a, int b) { return b; }

int main() {
    sink(id(source()));
    return 0;
}
