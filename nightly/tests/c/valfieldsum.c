/* A field sum stored back into the struct through a pointer (`a->x +
 * a->y`). The sum's value is computed from the tainted field, so it is
 * tainted; the struct's other fields are not, because adding two numbers
 * relates the sum to their values and not to the memory behind them.
 */

int source(void);
void sink(int x);

struct S {
  long a;
  long b;
  long sum;
  char *name;
};

static char clean[8] = "abc";

static void fill(struct S *s) {
  s->a = source();
  s->b = 2;
  s->name = clean;
  s->sum = s->a + s->b;
  sink((int)s->sum);
  sink(s->name[0]);
}

int main(void) {
  static struct S st;
  fill(&st);
  return 0;
}
