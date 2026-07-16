/* Taint that reaches the sink on only one path, the C analogue of
 * tests/java/BranchingFlow.java. `result` is tainted through the then-branch
 * and clean through the else-branch; the sink is reachable either way, so the
 * flow must still be reported (the analysis is not path sensitive).
 *
 * Only the source and the sink are in expected_lines: the tainted branch's
 * assignment is on the flow, but at -O0 no reported instruction maps back to
 * it.
 */

int source(void);
void sink(int x);

int main(int argc, char **argv) {
  (void)argv;
  int data = source();
  int result;
  if (argc > 1) {
    result = data;
  } else {
    result = 0;
  }
  sink(result);
  return 0;
}
