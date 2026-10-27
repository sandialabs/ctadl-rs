/* Precision at call sites and through operands: every `sink_clean*` must stay
 * silent, every `sink_hit*` must be reached. Each clean sink reads a value that
 * shares a call site, an expression or a struct with a tainted one but holds
 * nothing from it. See `test_cli_query_c_no_callsite_smear` in tests/integration/cli.rs.
 */
int source(void);
char *source_buf(void);
void sink_hit_ret(unsigned long x);
void sink_clean_field(unsigned long x);
void sink_hit_prod(unsigned long x);
void sink_clean_operand(unsigned long x);
void sink_hit_mul(unsigned long x);
void sink_clean_nested(unsigned long x);
void sink_hit_buf(char *p);
void sink_clean_sibling(unsigned long x);

struct t {
  unsigned long width;
  unsigned long bps;
  char *buf;
};

unsigned long rowsize(struct t *obj) { return obj->width * 4; }
unsigned long mul(unsigned long a, unsigned long b) { return a * b; }

/* A call's return must not flow back onto the fields its callee read. */
void case_return(struct t *obj) {
  unsigned long w = obj->width;
  unsigned long r = rowsize(obj) + source();
  sink_hit_ret(r);
  sink_clean_field(w);
}

/* One operand of an expression must not taint the other. */
void case_operand(unsigned long m) {
  unsigned long n = source();
  unsigned long c = n * m;
  sink_hit_prod(c);
  sink_clean_operand(m);
}

/* A summary `ret <- a, ret <- b` must not make `a` taint `b`. */
void case_nested(struct t *obj) {
  unsigned long n = source();
  unsigned long s = mul(n, rowsize(obj));
  sink_hit_mul(s);
  sink_clean_nested(obj->width);
}

/* Saturation covers the stored field's subtree, not its siblings. */
void case_sibling(struct t *im) {
  im->buf = source_buf();
  sink_hit_buf(im->buf);
  sink_clean_sibling(im->bps);
}
