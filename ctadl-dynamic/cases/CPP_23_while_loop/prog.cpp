/* Taint assigned inside a while loop (deterministic: runs once). */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    int i = 0;
    while (i < 1) {
        x = s;
        i = i + 1;
    }
    sink(x);
    return 0;
}
