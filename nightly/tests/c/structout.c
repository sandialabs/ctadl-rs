/* Field precision across a summary that writes through a pointer parameter:
 * make() taints o->a and zeroes o->b, and the caller reads both fields back out
 * of the object whose address it passed. outparam.c does this with a scalar, so
 * it cannot say whether the two writes stay distinct; structptr.c writes a field
 * through a pointer parameter but its struct has only one field; globalstruct.c
 * pins two-field precision but through a global, where the addresses are static
 * rather than carried in through a formal.
 *
 * The sink on r.b must stay clean: the callee wrote it, but wrote it clean.
 * That line is listed in the query's `unexpected_lines`. This is also the
 * control for structret.c, which is this same round trip through a returned
 * value instead of an explicit pointer, and which does lose the distinction.
 */

int source(void);
void sink(int x);

struct wide {
  int a;
  int b;
  int pad[6];
};

static void make(struct wide *o) {
  o->a = source();
  o->b = 0;
}

int main(void) {
  struct wide r;
  make(&r);
  sink(r.b);
  sink(r.a);
  return 0;
}
