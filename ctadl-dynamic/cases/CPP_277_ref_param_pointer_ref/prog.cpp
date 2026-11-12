/* C++ reference parameters after #154, POSITIVE: a reference to a pointer, `int*& p`, rebinds the caller's pointer to `&y`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void bind(int*& p, int* q) { p = q; }
int main() { int y = source(); int* p = 0; bind(p, &y); sink(*p); return 0; }
