/* Taint stored through pointer arithmetic and read back through a subscript:
 * `*(p + 2) = source()` then `arr[2]`. arrayflow.c uses a subscript on both
 * ends of the round trip; here the store never names `arr` at all, so the flow
 * exists only if the analysis resolves `p + 2` to the same slot that `arr[2]`
 * names.
 *
 * The sink on arr[0] must stay clean: the store went to a different element.
 * That line is listed in the query's `unexpected_lines`, so losing index
 * precision fails the case rather than going unnoticed.
 */

int source(void);
void sink(int x);

int main(void) {
  int arr[4] = {0, 0, 0, 0};
  int *p = arr;
  *(p + 2) = source();
  sink(arr[0]);
  sink(arr[2]);
  return 0;
}
