/* C++ reference results through pointers and with several places (spec 049), NEGATIVE: `int& at(int* a) { return a[1]; }`; `at(arr) = source(); sink(arr[0])`: only element 1 is written. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int& at(int* a) { return a[1]; }
int main() { int arr[2] = {0, 0}; at(arr) = source(); sink(arr[0]); return 0; }
