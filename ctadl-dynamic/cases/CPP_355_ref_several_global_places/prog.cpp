/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = pickg(1); r = source(); sink(ga)`, where `pickg` returns either of two globals. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int ga = 0; int gb = 0;
int& pickg(int c) { if (c) return ga; return gb; }
int main() { int& r = pickg(1); r = source(); sink(ga); return 0; }
