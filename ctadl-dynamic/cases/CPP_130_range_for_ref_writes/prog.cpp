/* C++ range-based for (spec 026), POSITIVE (`r3`/`r4`): a reference loop variable aliases the
   element, so writing through it writes the array; a second, by-value loop reads it back. Both
   `int&` and `auto&` spellings. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int a[2] = {0, 0};
    for (auto& y : a) {
        y = source();
    }
    int s = 0;
    for (int& x : a)
        s = s + x;
    sink(s);
    return 0;
}
