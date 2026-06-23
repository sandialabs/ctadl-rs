/* F1 boundary: function pointer stored in a STRUCT FIELD and called through it. */
int source();
void sink(int);

int id(int p) { return p; }

struct Ops {
    int (*op)(int);
};

int main() {
    struct Ops o;
    o.op = id;
    int s = source();
    int r = o.op(s);
    sink(r);
    return 0;
}
