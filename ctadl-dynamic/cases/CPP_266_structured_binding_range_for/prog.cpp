/* C++ structured bindings (spec 045), POSITIVE (`b2_range_for_binding`). `for (auto& [k, v] : arr)` aliases the element's members. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int a; int b; };
int main() { P arr[2]; arr[0].a = source(); arr[0].b = 0; arr[1].a = 0; arr[1].b = 0; int s = 0; for (auto& [k, v] : arr) s = s + k; sink(s); return 0; }
