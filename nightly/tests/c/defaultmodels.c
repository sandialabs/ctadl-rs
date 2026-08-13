/* Exercises the SHIPPED native propagation defaults
 * (`ctadl-ascent/src/models/defaults/native-index.jsonl`).
 *
 * This case supplies no propagation model of its own -- only a source on
 * produce() and a sink on consume(). Every step between them is a libc
 * function with no body in this binary, so the chain exists only if the
 * default file is loaded for a Native import and models:
 *
 *   strcpy    Argument(1).deref -> Argument(0).deref
 *   strcat    Argument(1).deref -> Argument(0).deref
 *   strdup    Argument(0).deref -> Return.deref
 *
 * Break the defaults and this case goes quiet, which nothing else in the
 * suite would say.
 *
 * Deliberately fixed-arity: `snprintf` would be the obvious third step, but
 * Ghidra drops its variadic arguments on a Mach-O object, so a case built on
 * it passes on Linux and reports nothing on a developer's macOS box.
 *
 * The source and sink ports are `Return.deref` / `Argument(0).deref`, not the
 * bare ports the other C cases use, because what is tainted here is the string
 * BEHIND the pointer -- which is also the level every model above reads and
 * writes at.
 */

#include <stdlib.h>
#include <string.h>

const char *produce(void);
void consume(const char *x);

void handler(void) {
  char buf[64];

  strcpy(buf, produce());
  strcat(buf, "!");
  char *copy = strdup(buf);
  consume(copy);
  free(copy);
}

int main(void) {
  handler();
  return 0;
}
