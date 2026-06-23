/* Taint flows through a GLOBAL variable within one function. */
int source();
void sink(int);

int g;

int main() {
    g = source();
    sink(g);
    return 0;
}
