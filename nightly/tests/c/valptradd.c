/* A store through `p + i` with a run-time index. `p` is the base of the
 * address, so the store reaches the memory behind it; `i` only picks the
 * offset. The analysis does not know `i`, so the store lands at `p`'s first
 * element, which is where `arr[0]` reads.
 */

int source(void);
void sink(int x);

static void put(int *p, int i) { *(p + i) = source(); }

int main(int argc, char **argv) {
  (void)argv;
  int arr[4] = {0, 0, 0, 0};
  put(arr, argc - 1);
  sink(arr[0]);
  return 0;
}
