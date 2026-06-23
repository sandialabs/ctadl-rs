/* Taint read back through a pointer alias: p points at x, then *p is read. */
int source();
void sink(int);

int main() {
    int x = source();
    int *p = &x;
    int y = *p;
    sink(y);
    return 0;
}
