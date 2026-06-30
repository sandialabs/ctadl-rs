/* Conditional, taint branch ALWAYS taken (if(1)). Both static and dynamic should
   see the flow. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    if (1) {
        x = s;
    }
    sink(x);
    return 0;
}
