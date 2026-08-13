/* C++ mirror of case 36. Function-pointer array built by an aggregate (brace) initializer:
   `int (*fps[2])(int) = { id, id };` then an indirect call through `fps[0]`. Each element lowers
   through the same `fps.[i]` store the element-assignment form uses (CPP_30), so codegen emits the
   `func_ptr_assign` per element and the indirect callee resolves. Only `source`/`sink` need
   `extern "C"` (the markers model and the DFSan shim match them by unmangled name); `id` is an
   ordinary C++ function, exactly as in CPP_30. DFSan observes the flow: `s=flow d=flow agree`.
   Spec 019. */
extern "C" int source();
extern "C" void sink(int);

int id(int p) { return p; }

int main() {
    int (*fps[2])(int) = { id, id };
    int s = source();
    int r = fps[0](s);
    sink(r);
    return 0;
}
