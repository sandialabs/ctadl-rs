/* Interprocedural: taint flows through TWO direct calls (b -> a), so the summaries
   must compose across the chain. */
int source();
void sink(int);

int a(int p) { return p; }
int b(int p) { return a(p); }

int main() {
    int s = source();
    int r = b(s);
    sink(r);
    return 0;
}
