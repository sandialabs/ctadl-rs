/* Taint flows through a function (id returns its argument): exercises the
   interprocedural function summary (return <- arg0). */
int source();
void sink(int);

int id(int p) {
    return p;
}

int main() {
    int s = source();
    int r = id(s);
    sink(r);
    return 0;
}
