/* Interprocedural taint chain through an intermediate function:
 *   produce() returns a value      -> upward flow into middle()'s local
 *   middle() hands it to consume() -> downward flow into the sink argument
 * The value also crosses a local-to-local copy in between, so this pins that
 * taint survives a return, a plain assignment, and a call argument in one
 * chain. The copy itself is not in expected_lines: it is on the flow (the
 * consume() call could not be tainted otherwise) but no reported instruction
 * maps back to that line at -O0.
 */

int produce(void);
void consume(int x);

void middle(void) {
  int x = produce(); /* upward flow: return of produce() -> local x */
  int y;
  y = x;
  consume(y); /* downward flow: y passed to consume() */
}

int main(void) {
  middle();
  return 0;
}
