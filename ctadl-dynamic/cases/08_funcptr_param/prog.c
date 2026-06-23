/* F1 boundary: function pointer passed AS A PARAMETER and called through that
   parameter. */
int source();
void sink(int);

int id(int p) { return p; }

int apply(int (*f)(int), int x) {
    return f(x);
}

int main() {
    int s = source();
    int r = apply(id, s);
    sink(r);
    return 0;
}
