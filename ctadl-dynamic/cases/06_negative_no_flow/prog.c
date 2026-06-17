/* Negative control: source() and sink() both present, but no data path between
   them — the sink receives an untainted constant. Expect NO flow. */
int source();
void sink(int);

int main() {
    int s = source();
    int x = 0;
    sink(x);
    return 0;
}
