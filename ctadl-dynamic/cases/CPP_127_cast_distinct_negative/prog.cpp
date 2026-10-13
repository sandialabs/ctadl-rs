/* C++ casts (spec 025), NEGATIVE (`q6_static_cast_distinct` + `q11_functional_cast_distinct`):
   the cast operands are the untainted `y`, so nothing flows. The precision guard: it fails
   (s=flow) if a cast stops being exactly its operand and starts blending in anything else. s=none
   d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int x = source();
    int y = 0;
    sink(static_cast<int>(y) + int(y));
    return x - x;
}
