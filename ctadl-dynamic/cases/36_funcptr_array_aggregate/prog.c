/* Function-pointer array built by an aggregate (brace) initializer:
   `int (*fps[2])(int) = { id, id };` then an indirect call through `fps[0]`. The brace form
   must produce exactly the facts the element-assignment form does (case 30: `fps[0] = id;
   fps[1] = id;`) -- each element lowers through the same `fps.[i]` store, so codegen emits the
   `func_ptr_assign` per element and the F2 propagation resolves the indirect callee.
   This shape is what originally hid F2: the generator had to avoid brace initializers because
   they failed ingestion (case 31), which masked the funcptr-array gap. DFSan observes the flow:
   `s=flow d=flow agree`. Spec 019. */
int source();
void sink(int);

int id(int p) { return p; }

int main() {
    int (*fps[2])(int) = { id, id };
    int s = source();
    int r = fps[0](s);
    sink(r);
    return 0;
}
