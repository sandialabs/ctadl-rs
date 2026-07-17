/* Taint accumulated across a loop body, the C analogue of
 * tests/java/LoopFlow.java. `acc` starts clean and is only tainted by the
 * loop-carried `acc += data`, so the sink line being reported is what pins
 * that taint survives a back edge instead of being dropped after one pass
 * over the body.
 */

int source(void);
void sink(int x);

int main(void) {
  int data = source();
  int acc = 0;
  for (int i = 0; i < 5; i++) {
    acc += data;
  }
  sink(acc);
  return 0;
}
