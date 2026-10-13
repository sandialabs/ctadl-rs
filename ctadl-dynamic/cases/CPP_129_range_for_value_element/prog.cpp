/* C++ range-based for (spec 026), POSITIVE (`r2_value_reads_element0`): a by-value loop variable
   is a copy of an element of the range -- the location a non-constant `a[i]` names -- so the
   tainted `a[0]` reaches the sink. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int a[3] = {0, 0, 0};
    a[0] = source();
    for (int x : a)
        sink(x);
    return 0;
}
