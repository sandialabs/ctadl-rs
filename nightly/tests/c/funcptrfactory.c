/* Indirect call through a function pointer manufactured by a factory: the
 * target is stored in `lookup`'s frame and reaches `main` only by riding the
 * return value. funcptr.c covers a function pointer stored in a local of the
 * calling frame and vtable.c one loaded back out of a struct field; in both
 * the call-target fact and the indirect call site live in the same function.
 * Here they do not, so resolving `h(data)` requires the call target to survive
 * the return boundary (`h = lookup(); h(...)`).
 *
 * Only the source and the sink are in expected_lines; the indirect call site
 * itself maps to no reported instruction at -O0.
 */

int source(void);
void sink(int x);

typedef int (*transform_fn)(int);

static int passthrough(int v) {
  return v;
}

/* Factory: the returned target has no in-formal source, so it cannot reach
 * the caller through a pass-through summary. */
static transform_fn lookup(void) {
  return passthrough;
}

int main(void) {
  transform_fn h = lookup();
  int data = source();
  int result = h(data);
  sink(result);
  return 0;
}
