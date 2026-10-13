/* F6 (OPEN, known gap): a `static` local keeps its value across calls (static storage
   duration), but the frontend lowers it as an ordinary local, so the value the first call
   stores is gone in the second. d=flow, s=none. */
int source();
void sink(int);

int next(int x) {
    static int last = 0;
    int r = last;
    last = x;
    return r;
}

int main() {
    next(source());
    sink(next(0));
    return 0;
}
