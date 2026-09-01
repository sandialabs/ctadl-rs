/* C++ pointer-returning method (spec 130 parity for the C++ discovery paths), POSITIVE.
   `int *pick(int *p)` wraps its `function_declarator` in a `pointer_declarator`. Method
   discovery used to demand the bare `function_declarator` (or a `reference_declarator` around
   it) and silently dropped every pointer-returning method body -- inline, out-of-line and
   namespaced alike -- so a call to one resolved to an empty extern stub and the taint that
   went in did not come out. With the body lowered, `pick` returns its pointer parameter: `q`
   is `arr`, so `sink(q[0])` reads the tainted `arr[0]`. DFSan observes the same flow at
   runtime: s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int *pick(int *p) { return p; }   /* pointer-returning method: body used to be dropped */
};

int main() {
    Box b;
    int arr[2] = {0, 0};
    arr[0] = source();       /* taints arr[0] */
    int *q = b.pick(arr);    /* the method hands the array back through its pointer result */
    sink(q[0]);              /* reads the tainted element through the returned pointer -- flows */
    return 0;
}
