/* Indirect call through a function pointer held in a struct field: the C
 * analogue of interface dispatch in tests/java/MethodCallFlow.java. funcptr.c
 * already covers a function pointer in a local; here the callee is reachable
 * only by loading it back out of a struct field, which is the shape a
 * hand-rolled vtable has.
 *
 * Only the source and the sink are in expected_lines; the indirect call site
 * itself maps to no reported instruction at -O0.
 */

int source(void);
void sink(int x);

struct processor {
  int (*process)(int);
};

static int passthrough(int v) {
  return v;
}

int main(void) {
  struct processor p;
  p.process = passthrough;
  int data = source();
  int result = p.process(data);
  sink(result);
  return 0;
}
