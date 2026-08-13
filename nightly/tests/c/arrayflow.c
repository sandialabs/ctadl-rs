/* Taint into an array element and back out again, the C analogue of
 * tests/java/ArrayFlow.java:
 *   arr[1] = source() -> store of a tainted value into an array slot
 *   local  = arr[1]   -> load from that slot
 *   sink(local)       -> the loaded value reaches the sink
 * This pins that taint survives a store/load round trip through an array,
 * which the struct-field case in example.c does not cover.
 *
 * As in callchain.c, the load is on the flow but is not in expected_lines: at
 * -O0 no reported instruction maps back to that line.
 */

int source(void);
void sink(int x);

int main(void) {
  int arr[3];
  arr[1] = source();
  int local = arr[1];
  sink(local);
  return 0;
}
