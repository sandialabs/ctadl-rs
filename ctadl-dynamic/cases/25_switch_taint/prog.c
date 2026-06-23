/* Taint assigned in the selected `switch` case (deterministic: switch(1) -> case 1).
   Also probes whether the C frontend ingests `switch` at all. */
int source();
void sink(int);

int main() {
    int s = source();
    int x = 0;
    switch (1) {
        case 1:
            x = s;
            break;
        default:
            x = 0;
            break;
    }
    sink(x);
    return 0;
}
