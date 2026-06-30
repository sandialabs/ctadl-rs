/* Taint hops through an intermediate local variable before reaching the sink. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int t = s;
    sink(t);
    return 0;
}
