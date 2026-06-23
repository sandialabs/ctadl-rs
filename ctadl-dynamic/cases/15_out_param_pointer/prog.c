/* Output parameter: set() writes the tainted value through a pointer parameter
   (`*out = v`), and the caller reads the result. Scalar analog of xfer.c (no array). */
int source();
void sink(int);

void set(int *out, int v) {
    *out = v;
}

int main() {
    int x;
    set(&x, source());
    sink(x);
    return 0;
}
