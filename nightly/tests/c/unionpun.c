/* Taint through overlapping union members: source() is written through
 * `as_int` and read back through `as_uint`, which is the same storage under a
 * different name. globalstruct.c pins the opposite property -- that two struct
 * fields at different offsets stay distinct -- so this pins the complement,
 * that the analysis does not over-separate members which genuinely alias. A
 * model keyed on field names rather than offsets would report nothing here.
 */

int source(void);
void sink(int x);

union punner {
  int as_int;
  unsigned as_uint;
};

int main(void) {
  union punner u;
  u.as_int = source();
  sink((int)u.as_uint);
  return 0;
}
