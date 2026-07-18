/* Field-sensitive taint through a global aggregate, extending globalflow.c from a
 * scalar global to one field of a struct:
 *   produce() writes source() into g_pair.a
 *   consume() reads g_pair.a and passes it to the sink
 * As in globalflow.c neither function passes anything to the other, so the flow
 * exists only if the analysis tracks taint through the global's storage across
 * calls -- but here it must also distinguish the two fields, which differ only in
 * their address within the same global.
 *
 * The sink on g_pair.b must stay clean: it names the field that was never written.
 * That line is listed in the query's `unexpected_lines`, so taint reaching it fails
 * the case rather than going unnoticed.
 */

int source(void);
void sink(int x);

struct pair {
  int a;
  int b;
};

struct pair g_pair;

void produce(void) {
  g_pair.a = source();
}

void consume(void) {
  sink(g_pair.b);
  sink(g_pair.a);
}

int main(void) {
  produce();
  consume();
  return 0;
}
