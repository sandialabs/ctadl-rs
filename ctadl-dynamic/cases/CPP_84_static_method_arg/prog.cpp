/* C++ static member function (spec 015, FR-2/FR-3), POSITIVE. `C::identity` is a `static`
   member function: it has NO implicit `this`, so its only parameter is the declared `x`, and
   `C::identity(args)` is called like a namespaced free function (a `qualified_identifier` callee
   with no receiver). Before this slice CTADL lowered it with an implicit `this`, so the argument
   bound to the receiver at index 0 and the returned `x` (index 1) was never passed the source
   -- the flow was missed. Now the static method is receiver-less: `C::identity(source())` passes
   the source as its first (and only) parameter, which is returned straight to the sink. DFSan
   observes the argument reach the return: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct C {
    static int identity(int x) { return x; }        /* no `this`; the arg reaches the return */
};

int main() {
    sink(C::identity(source()));                     /* arg flows through the static method */
    return 0;
}
