/* Taint flows through a GLOBAL across functions: store() writes the tainted value
   into g, load() reads it back. */
int source();
void sink(int);

int g;

void store(int x) { g = x; }
int load() { return g; }

int main() {
    store(source());
    sink(load());
    return 0;
}
