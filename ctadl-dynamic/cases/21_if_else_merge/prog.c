/* if/else where the taken branch assigns the tainted value and the two paths merge
   before the sink. */
int source();
void sink(int);

int main() {
    int s = source();
    int x;
    if (1) {
        x = s;
    } else {
        x = 0;
    }
    sink(x);
    return 0;
}
