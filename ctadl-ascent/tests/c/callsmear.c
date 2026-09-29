/* sink_hit_* are real flows; sink_clean_* must stay silent. */
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
void sink_hit_retarg(unsigned long x);
void sink_clean_retarg(unsigned long x);
void sink_hit_field(unsigned long x);

struct t {
  unsigned long width;
  unsigned long bps;
  char *buf;
};

unsigned long rowsize(struct t *tif) { return tif->width * 4; }
unsigned long mul(unsigned long a, unsigned long b) { return a * b; }

/* Return does not taint fields the callee read. */
void case_return(struct t *tif) {
  unsigned long w = tif->width;
  unsigned long r = rowsize(tif) + source();
  sink_hit_ret(r);
  sink_clean_field(w);
}

/* Operands do not taint each other. */
void case_operand(unsigned long m) {
  unsigned long n = source();
  unsigned long c = n * m;
  sink_hit_prod(c);
  sink_clean_operand(m);
}

/* Args of one call do not taint each other. */
void case_nested(struct t *tif) {
  unsigned long n = source();
  unsigned long s = mul(n, rowsize(tif));
  sink_hit_mul(s);
  sink_clean_nested(tif->width);
}

/* Saturation does not reach sibling fields. */
void case_sibling(struct t *im) {
  im->buf = source_buf();
  sink_hit_buf(im->buf);
  sink_clean_sibling(im->bps);
}

/* Tainted return does not taint the field read (source: width_of). */
unsigned long width_of(struct t *p) { return p->width; }
void case_field(struct t *p) {
  unsigned long w = p->width;
  unsigned long s = width_of(p);
  sink_hit_field(s);
  sink_clean_field(w);
}

/* Tainted return does not taint the argument (source: wrap). */
unsigned long wrap(unsigned long n) { return n; }
void case_retarg(unsigned long m) {
  unsigned long t = wrap(m);
  sink_hit_retarg(t);
  sink_clean_retarg(m);
}
