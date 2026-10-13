/* C++ named casts (spec 025), POSITIVE (`q3_const_cast_write`): a callee casts away `const` and
   stores through the pointer; the store reaches the caller's `x`. The cast is in store position
   (`*const_cast<int*>(p) = ...`), so the location side resolves through it too. s=flow d=flow
   agree. */
extern "C" int source();
extern "C" void sink(int);

void put(const int* p) { *const_cast<int*>(p) = source(); }

int main() {
    int x = 0;
    put(&x);
    sink(x);
    return 0;
}
