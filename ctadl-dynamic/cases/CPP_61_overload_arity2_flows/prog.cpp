/* C++ overloading (by arity), free functions — POSITIVE (arity 2).
   `id` is overloaded: `id(int)` returns its one argument, `id(int,int)` returns its SECOND.
   The call `id(0, source())` has TWO explicit arguments, so it must resolve to the arity-2
   overload (`id#2`), which returns its second argument — so source (passed as the 2nd arg)
   reaches the sink, while the untainted 0 (1st arg) is dropped. This pins arity-2 selection and
   that taint follows exactly the selected overload's body. DFSan runs the arity-2 `id` (returns
   its 2nd arg), so it sees the flow (`s=flow d=flow`). */
extern "C" int source();
extern "C" void sink(int);

int id(int a) { return a; }
int id(int a, int b) { return b; }

int main() {
    sink(id(0, source()));
    return 0;
}
