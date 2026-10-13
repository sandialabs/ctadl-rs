/* C++ functional casts (spec 025), POSITIVE (`q8_functional_cast`): `long(x)` and `int(l)` are
   conversions, not calls. The grammar reads `int(l)` as a call to a `primitive_type` and `long(x)`
   as a call to an `identifier` named `long`; both must lower as their operand. s=flow d=flow
   agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int x = source();
    long l = long(x);
    sink(int(l));
    return 0;
}
