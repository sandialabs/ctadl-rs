/* C++ reference results as locations (spec 048), POSITIVE, KNOWN GAP F9: `int& at(int* a) { return a[1]; }`; `at(arr) = source(); sink(arr[1])`. A place through a pointer parameter is out of spec 048's scope. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& at(int* a) { return a[1]; }
int main() { int arr[2] = {0, 0}; at(arr) = source(); sink(arr[1]); return 0; }
