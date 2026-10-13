/* C++ range-based for (spec 026) x F3 (OPEN, known gap; `r10_nonzero_index_limitation`). The
   loop variable binds the location a non-constant `a[i]` names, which is `a[0]`'s path, so the
   write at `a[1]` is not seen -- the same shared-model gap as C case 37. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int a[3] = {0, 0, 0};
    a[1] = source();
    for (int x : a)
        sink(x);
    return 0;
}
