/* Taint flows through a GLOBAL variable within one function. */
extern "C" int source();
extern "C" void sink(int);

int g;

int main() {
    g = source();
    sink(g);
    return 0;
}
