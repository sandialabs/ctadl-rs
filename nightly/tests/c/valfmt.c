/* A tainted number formatted into a buffer that a command sink reads. The
 * digit is computed from the number, so the buffer's contents are derived
 * from its value, and the sink on the contents must see it.
 */

int source(void);
void run(const char *cmd);

static void fmt_digit(char *dst, int n) {
  dst[0] = (char)('0' + n % 10);
  dst[1] = 0;
}

int main(void) {
  char cmd[16];
  int n = source();
  fmt_digit(cmd, n);
  run(cmd);
  return 0;
}
