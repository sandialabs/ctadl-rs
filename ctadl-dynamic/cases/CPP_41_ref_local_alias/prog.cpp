/* C++ reference local alias: `int& r = x` binds `r` as another name for `x`'s storage, so
   reading `r` reads `x`. CTADL resolves `r` to `x`'s access path (an alias, not a copy), so
   `sink(r)` is `sink(x)` and the source taint flows. DFSan sees `r` and `x` share storage. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int x = source();
    int& r = x;
    sink(r);
    return 0;
}
