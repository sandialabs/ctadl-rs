/* C++ reference results as locations (spec 048), POSITIVE: `int& at(int* a) { return a[1]; }`; `at(arr) = source(); sink(arr[1])` writes what `arr[1]` names (spec 049; was known gap F9). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& at(int* a) { return a[1]; }
int main() { int arr[2] = {0, 0}; at(arr) = source(); sink(arr[1]); return 0; }
