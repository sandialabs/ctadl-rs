/* C++ named casts (spec 025), POSITIVE (`q2_reinterpret_roundtrip`): a pointer laundered through
   an integer and back with `reinterpret_cast` still names `x`, so the read through it carries
   the taint. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int x = source();
    long p = reinterpret_cast<long>(&x);
    int* q = reinterpret_cast<int*>(p);
    sink(*q);
    return 0;
}
