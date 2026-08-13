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
 * The difference is not the return itself but how the frontend models the pcode
 * PIECE/SUBPIECE ops (both routed through handle_binop in
 * languages/pcode/mod.rs, which unions its operands and discards the byte-offset
 * operand). At -O0 both copies move 8 bytes at a time across a and b together;
 * Ghidra fuses the two fields into one varnode with PIECE and slices them back
 * out with SUBPIECE(w, offset). Because handle_binop ignores the offset, taint
 * on either field spreads to the whole word and back to both fields.
 * structout.c instead reaches the fields through a pointer, so they stay
 * distinct .[off].deref access paths (LOAD/STORE keep the offset, PIECE/SUBPIECE
 * do not) and r.b stays clean. Adding padding so that a and b sit in different
 * 8-byte words also makes r.b come back clean, since the first-word PIECE no
 * longer mixes them. If SUBPIECE/PIECE ever become offset-aware, this test still
 * passes and the sink on r.b should be moved into `unexpected_lines`.
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
