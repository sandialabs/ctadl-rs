/* D4b (shape 5): the taint is passed as an argument *into* the resolved callee
   and consumed by a sink inside it.

   The mirror of funcptrcalleesource.c, and the same defect: `consumes` has no
   summary describing this (it returns nothing), so no summary instantiation at
   the dispatch site can carry the argument in. The flow needs a call *entry*
   edge at the dispatch instruction. */

int source();
void sink(int x);

typedef void (*fn1)(int);

void consumes(int x) {
  sink(x);
}

void run(fn1 g, int v) {
  g(v);
}

int main() {
  int y = source();
  run(consumes, y);
}
