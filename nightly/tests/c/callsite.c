/* Two call sites of one function, only one of them tainted. passthrough() is
 * summarized once as "argument 0 flows to the return value", and that summary
 * is then applied at both calls; the flow exists only if each application
 * carries its own call site's argument rather than the union of them.
 *
 * funcptr.c and vtable.c already call a passthrough, but each has a single call
 * site, so neither can catch a summary that leaks taint between them. The sink
 * on `clean` must stay clean: it is fed by the untainted call. That line is
 * listed in the query's `unexpected_lines`, so cross-contamination fails the
 * case rather than going unnoticed.
 */

int source(void);
void sink(int x);

static int passthrough(int v) {
  return v;
}

int main(void) {
  int tainted = passthrough(source());
  int clean = passthrough(0);
  sink(clean);
  sink(tainted);
  return 0;
}
