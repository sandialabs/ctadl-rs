/* Taint assigned in the default arm, reached because no explicit case matches
   (deterministic: switch(99) -> default). Exercises the valueless case_statement. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    switch (99) {
        case 1:
            x = 0;
            break;
        default:
            x = s;
            break;
    }
    sink(x);
    return 0;
}
