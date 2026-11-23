/* A string's length, `p - s`, used to index a clean buffer. The length is
 * computed from the pointer's value, not from the string's contents, so the
 * element read through it is clean. Lowered as a merge, the length would
 * alias the string's memory and taint every buffer it indexed.
 */

const char *produce(void);
void consume(int x);

static char clean[64];

static long length(const char *s) {
  const char *p = s;
  while (*p)
    p++;
  return p - s;
}

int main(void) {
  const char *s = produce();
  long n = length(s);
  consume(clean[n & 63]);
  consume(s[0]);
  return 0;
}
