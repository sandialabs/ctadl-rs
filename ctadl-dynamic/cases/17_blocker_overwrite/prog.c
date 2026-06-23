/* BLOCKER (expect no flow): the tainted value is overwritten by a constant before
   the sink. SSA should see the sink read the constant, not the source. */
int source();
void sink(int);

int main() {
    int s = source();
    s = 0;
    sink(s);
    return 0;
}
