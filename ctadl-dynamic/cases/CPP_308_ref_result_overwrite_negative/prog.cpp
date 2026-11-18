/* C++ reference results as locations (spec 048), NEGATIVE: `pick(x) = source(); x = 0; sink(x)`: the later strong update kills it. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int& pick(int& v) { return v; }
int main() { int x = 0; pick(x) = source(); x = 0; sink(x); return 0; }
