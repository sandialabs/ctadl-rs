/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `set(c ? x : y); sink(x)`, where `set` writes its `int&`: the cell is copied back weakly into both arms. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void set(int& o) { o = source(); }
int main() { int x = 0; int y = 0; int c = 1; set(c ? x : y); sink(x); return 0; }
