/* Taint assigned inside a for loop (deterministic: runs once). */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    for (int i = 0; i < 1; i = i + 1) {
        x = s;
    }
    sink(x);
    return 0;
}
