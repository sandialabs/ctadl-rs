/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `(c ? x : y) = source(); sink(x)` with no call: a conditional of two lvalues names both, written weakly. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int main() { int x = 0; int y = 0; int c = 1; (c ? x : y) = source(); sink(x); return 0; }
