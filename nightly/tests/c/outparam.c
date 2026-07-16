/* Taint handed back through an out-parameter instead of a return value. Java
 * has no equivalent (the closest is a setter, InstanceMethodFlow.java), but it
 * is the usual way a C function returns data: fill() taints *out, and the
 * caller reads the local whose address it passed. Unlike example.c, the source
 * is called inside the callee, so the taint originates below the caller and
 * flows up through the pointer.
 */

int source(void);
void sink(int x);

void fill(int *out) {
  *out = source();
}

int main(void) {
  int v;
  fill(&v);
  sink(v);
  return 0;
}
