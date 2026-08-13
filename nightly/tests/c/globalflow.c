/* Taint through a file-scope global, the C analogue of
 * tests/java/StaticFieldFlow.java and CrossClassStaticFieldFlow.java:
 *   produce() writes source() into the global
 *   consume() reads the global and passes it to the sink
 * Neither function passes anything to the other, so the flow exists only if
 * the analysis tracks taint through the global's storage across calls.
 */

int source(void);
void sink(int x);

int g_data;

void produce(void) {
  g_data = source();
}

void consume(void) {
  sink(g_data);
}

int main(void) {
  produce();
  consume();
  return 0;
}
