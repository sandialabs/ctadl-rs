/* Recursion: a recursive identity passes the tainted value down to the base case
   and back up. Deterministic (fixed depth 2), no user input. */
extern "C" int source();
extern "C" void sink(int);

int rec(int n, int x) {
    if (n == 0) {
        return x;
    }
    return rec(n - 1, x);
}

int main() {
    int s = source();
    int r = rec(2, s);
    sink(r);
    return 0;
}
