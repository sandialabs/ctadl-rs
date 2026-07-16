/* Taint through a whole-struct assignment: `dst = src` copies the aggregate in
 * one statement that names no field at all. At -O0 this lowers to a block copy
 * of the struct's bytes, so the flow exists only if the analysis carries taint
 * across a copy that never mentions `.a`. example.c and structptr.c both move
 * taint through a field by naming it; this moves it without.
 *
 * The sink on dst.b must stay clean: field precision has to survive the copy,
 * not just reachability. Treating the aggregate as one opaque blob would taint
 * both fields, so dst.b is listed in the query's `unexpected_lines`.
 */

int source(void);
void sink(int x);

struct rec {
  int a;
  int b;
};

int main(void) {
  struct rec src;
  src.a = source();
  src.b = 0;
  struct rec dst = src;
  sink(dst.b);
  sink(dst.a);
  return 0;
}
