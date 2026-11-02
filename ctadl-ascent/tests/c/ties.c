/* Taint traces with ties, for `determinism.rs`: two labels, several sources and sinks per
 * label, and equal-length paths between them, so which trace is reported and the order
 * everything is printed in both hang on tie-breaks. Names are chosen so that content order
 * and the order the frontend first sees them disagree.
 */
int src_zeta(void);
int src_alpha(void);
void sink_omega(int x);
void sink_beta(int x);

int pass_y(int v) { return v; }
int pass_b(int v) { return v; }

/* Two equal-length paths from one source to each sink. */
void zz_diamond(int c) {
  int s = src_zeta();
  int left = pass_y(s);
  int right = pass_b(s);
  int m = c ? left : right;
  sink_omega(m);
  sink_beta(left + right);
}

/* Two labels meeting at the same sinks. */
void aa_fanin(void) {
  int q = src_alpha();
  int p = src_zeta();
  int y = q;
  int b = p;
  sink_beta(y + b);
  sink_omega(b + y);
}

/* Several access paths under one variable, so the order paths are printed in hangs on how
 * they compare. */
struct inner {
  int ii;
  int *ip;
};
struct pair {
  int zz;
  int aa;
  struct inner in;
  int mm;
};

void put_zz(struct pair *p, int v) { p->zz = v; }
void put_in(struct pair *p, int v) { p->in.ii = v; *p->in.ip = v; }
int get_mm(struct pair *p) { return p->mm + p->in.ii; }

void mm_fields(struct pair *r) {
  put_zz(r, src_zeta());
  put_in(r, src_alpha());
  r->aa = *r->in.ip;
  r->mm = r->zz + r->aa;
  sink_beta(get_mm(r));
  sink_omega(r->zz + *r->in.ip);
}
