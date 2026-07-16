/* Taint in a struct returned by value. `struct wide` is larger than 16 bytes,
 * so the x86-64 ABI returns it through a hidden pointer the caller passes in:
 * make() has no parameters in the source, but the flow only exists if the
 * analysis follows the sret pointer it never sees written down. Every other
 * interprocedural case here returns a scalar in a register (callchain.c,
 * funcptr.c, recursion.c) or takes an explicit pointer parameter (outparam.c,
 * structout.c).
 *
 * KNOWN IMPRECISION: r.b is reported tainted here even though make() writes it
 * clean, which is why it is not in `unexpected_lines` -- structout.c is the
 * same round trip through an explicit out-parameter and does keep r.b clean.
 * The difference is not the return: at -O0 both copies move 8 bytes at a time
 * across a and b together, but structout.c's writes land in a stack slot the
 * frontend decomposes per field, whereas the sret copy goes through a pointer
 * and the taint lands on the whole object. Adding padding so that a and b sit
 * in different 8-byte words makes r.b come back clean. If a fix ever restores
 * field precision through a returned aggregate, this test still passes and the
 * sink on r.b should be moved into `unexpected_lines`.
 */

int source(void);
void sink(int x);

struct wide {
  int a;
  int b;
  int pad[6];
};

static struct wide make(void) {
  struct wide w;
  w.a = source();
  w.b = 0;
  return w;
}

int main(void) {
  struct wide r = make();
  sink(r.b);
  sink(r.a);
  return 0;
}
