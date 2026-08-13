/* Taint carried through a recursive call chain: descend() hands its tainted
 * argument to itself until the base case returns it back up. callchain.c pins a
 * flow across a finite chain of distinct functions; here the call graph has a
 * cycle, so the analysis must reach a fixed point on descend()'s own summary
 * instead of walking the chain to its end. loopflow.c is the intraprocedural
 * analogue of this (a back edge in the CFG); this is the interprocedural one.
 *
 * Only the source and the sink are in expected_lines: the recursive call is on
 * the flow but at -O0 no reported instruction maps back to that line.
 */

int source(void);
void sink(int x);

static int descend(int depth, int v) {
  if (depth == 0) {
    return v;
  }
  return descend(depth - 1, v);
}

int main(void) {
  int data = source();
  int result = descend(3, data);
  sink(result);
  return 0;
}
