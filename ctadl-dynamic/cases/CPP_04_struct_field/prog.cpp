/* Taint flows into a struct field and the sink reads that same field:
   exercises field sensitivity. */
extern "C" int source();
extern "C" void sink(int);

struct S {
    int f;
};

int main() {
    struct S x;
    x.f = source();
    sink(x.f);
    return 0;
}
