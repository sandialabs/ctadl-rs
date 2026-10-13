/* C++ structured bindings (spec 045), POSITIVE (`b6_array_binding`). `auto [a, b] = arr;` binds elements by index. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int arr[2] = {0, 0}; arr[1] = source(); auto [a, b] = arr; sink(b); return 0; }
