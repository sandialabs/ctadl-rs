struct bar { int c; int d; };
struct foo { int a; struct bar b; };

int source();
void sink(int x);
void transfer(struct bar* out, int val);

int main() {
  struct foo x;
  int y = source();
  transfer(&x.b, y);
  sink(x.b.d);
}

void transfer(struct bar* out, int val) {
  // y flows to val which flows to out.d
  // out is &x.b
  out->d = val;
}
