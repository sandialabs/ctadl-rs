/* C++ named casts (spec 025), POSITIVE (`q1_static_cast_value`). tree-sitter-cpp parses
   `static_cast<long>(e)` as a call whose callee is a `template_function`, so it used to lower as a
   call to an undefined function `static_cast<long>` and drop the operand. A named cast is
   value-preserving: its value is its operand. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    long l = static_cast<long>(source());
    sink(static_cast<int>(l));
    return 0;
}
