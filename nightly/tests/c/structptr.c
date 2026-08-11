/* Taint in and out of a struct field through accessor functions, the C
 * analogue of tests/java/InstanceMethodFlow.java:
 *   set_data() stores a tainted argument into h->data  (downward, into a field)
 *   get_data() returns h->data                         (upward, out of a field)
 * example.c already covers a store through a pointer parameter; the new part
 * here is the read back out through a separate call.
 */

struct holder {
  int data;
};

int source(void);
void sink(int x);

void set_data(struct holder *h, int v) {
  h->data = v;
}

int get_data(struct holder *h) {
  return h->data;
}

int main(void) {
  struct holder h;
  set_data(&h, source());
  int retrieved = get_data(&h);
  sink(retrieved);
  return 0;
}
