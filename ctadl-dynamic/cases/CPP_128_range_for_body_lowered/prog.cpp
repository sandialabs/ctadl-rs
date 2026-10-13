/* C++ range-based for (spec 026), POSITIVE (`r1_body_is_lowered`). A `for_range_loop` used to
   fall into the statement walker's expression catch-all, which dropped the whole loop -- body
   included -- so even `sink(source())` inside it vanished. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int a[2] = {1, 2};
    for (int x : a) {
        sink(source() + x);
    }
    return 0;
}
