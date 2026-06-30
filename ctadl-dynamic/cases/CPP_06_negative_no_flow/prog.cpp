/* Negative control: source() and sink() both present, but no data path between
   them — the sink receives an untainted constant. Expect NO flow. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    sink(x);
    return 0;
}
