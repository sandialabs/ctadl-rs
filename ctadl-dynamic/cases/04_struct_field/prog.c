/* Taint flows into a struct field and the sink reads that same field:
   exercises field sensitivity. */
int source();
void sink(int);

struct S {
    int f;
};

int main() {
    struct S x;
    x.f = source();
    sink(x.f);
    return 0;
}
