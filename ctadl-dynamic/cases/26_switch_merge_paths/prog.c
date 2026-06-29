/* Multiple switch arms; the taken case is tainted and the paths merge before the
   sink (deterministic: switch(1) -> case 1). Mirrors 21_if_else_merge. */
int source();
void sink(int);

int main() {
    int s = source();
    int x;
    switch (1) {
        case 1:
            x = s;
            break;
        case 2:
            x = 0;
            break;
        default:
            x = 0;
            break;
    }
    sink(x);
    return 0;
}
