/* Taint into one field of one element of an array of structs: `a[1].y` is a
 * single slot named by composing an array index with a field offset, which at
 * -O0 lowers to one address computation (1 * sizeof(struct pt) + offsetof(y)).
 * arrayflow.c and ptrarith.c index an array of scalars; globalstruct.c and
 * structcopy.c pin field offsets with no index. This is the composition of the
 * two, which neither covers.
 *
 * Both other sinks must stay clean, and they fail differently: a[1].x shares
 * the element but not the field, and a[0].y shares the field but not the
 * element. Collapsing either the index or the offset lights exactly one of
 * them up, so the two `unexpected_lines` entries distinguish which kind of
 * precision was lost.
 */

int source(void);
void sink(int x);

struct pt {
  int x;
  int y;
};

int main(void) {
  struct pt a[3];
  a[1].y = source();
  sink(a[1].x);
  sink(a[0].y);
  sink(a[1].y);
  return 0;
}
