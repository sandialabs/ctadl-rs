/* Taint hops through an intermediate local variable before reaching the sink. */
int source();
void sink(int);

int main() {
    int s = source();
    int t = s;
    sink(t);
    return 0;
}
