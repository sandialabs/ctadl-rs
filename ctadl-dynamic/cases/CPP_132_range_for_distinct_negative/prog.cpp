/* C++ range-based for (spec 026), NEGATIVE (`r8_distinct_negative`): the range holds only
   constants and the tainted `t` is never stored into it, so the sum carries nothing. The
   precision guard: it fails (s=flow) if the loop variable starts blending anything beyond the
   range's elements. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int t = source();
    int a[2] = {0, 0};
    int s = 0;
    for (int x : a)
        s = s + x;
    sink(s);
    return t - t;
}
