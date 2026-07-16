/* Taint in a struct passed by value that the ABI passes in memory. `struct
 * wide` is larger than 16 bytes, so the x86-64 SysV ABI classifies it MEMORY
 * and the caller copies it into the outgoing argument area on the stack rather
 * than handing it over in registers. A struct that fits in two eightbytes goes
 * in registers instead and is a genuinely different path through the frontend;
 * structret.c returns such a struct, but nothing else here passes one in by
 * value through the stack.
 *
 * This is the case the by-value-through-memory fix restored: the callee reads
 * the argument out of a stack slot the caller wrote, so the flow only exists if
 * the stack parameter is bound to that slot (and bound to its contents, not its
 * address). Before the fix this produced no tainted instructions at all.
 *
 * KNOWN IMPRECISION: sink(w.b) is reported tainted here even though w.b is
 * written clean, which is why it is not in `unexpected_lines`. It is the same
 * wide-copy imprecision documented in structret.c -- at -O0 the caller copies
 * the aggregate into the argument area 8 bytes at a time, spanning a and b
 * together, and the taint lands on the whole slot. structout.c is the
 * field-precise control (same round trip through an explicit pointer, w.b stays
 * clean). If a fix ever restores field precision through a stack-passed
 * aggregate, this test still passes and the sink on w.b should move into
 * `unexpected_lines`.
 */

int source(void);
void sink(int x);

struct wide {
  int a;
  int b;
  int pad[6];
};

static void consume(struct wide w) {
  sink(w.b);
  sink(w.a);
}

int main(void) {
  struct wide w;
  w.a = source();
  w.b = 0;
  consume(w);
  return 0;
}
