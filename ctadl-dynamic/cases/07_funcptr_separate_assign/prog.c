/* F1 boundary: function pointer ASSIGNED separately (not initialized at the
   declaration), then called indirectly. Probes whether the gap depends on the
   declaration-initializer form. */
int source();
void sink(int);

int id(int p) { return p; }

int main() {
    int (*fp)(int);
    fp = id;
    int s = source();
    int r = fp(s);
    sink(r);
    return 0;
}
