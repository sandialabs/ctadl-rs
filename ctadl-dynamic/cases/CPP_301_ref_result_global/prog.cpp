/* C++ reference results as locations (spec 048), POSITIVE: `int& gref() { return g; }`; `gref() = source(); sink(g)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int g = 0;
int& gref() { return g; }
int main() { gref() = source(); sink(g); return 0; }
