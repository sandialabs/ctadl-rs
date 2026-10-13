/* F3 (OPEN, known gap): a non-constant subscript `a[i]` lowers to the same path as `a[0]`
   (`a.deref`; offset 0 is elided), so it never meets a write at a nonzero constant index
   (`a.[1].deref`). The loop reads `a[1]` at runtime: d=flow, s=none. The fix needs an
   "unknown offset" in the IR path model that may-aliases every concrete offset. */
int source();
void sink(int);

int main() {
    int a[3] = {0, 0, 0};
    a[1] = source();
    for (int i = 0; i < 3; i++)
        sink(a[i]);
    return 0;
}
