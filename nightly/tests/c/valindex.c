/* An index into a table. The load reads the table's memory, and the tainted
 * index only picks the element, so the element is clean: taint through an
 * index is not followed, by design. The index's own value stays tainted.
 */

int source(void);
void sink(int x);

static const char table[16] = "0123456789abcdef";

int main(void) {
  int i = source();
  char c = table[i & 15];
  sink(c);
  sink(i + 1);
  return 0;
}
