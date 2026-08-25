/* D4 (shape 2): the function pointer arrives from a caller, and the flow is
   consumed inside the frame that holds the indirect call.

   The engine resolves the call and derives a contextual assignment for it, but
   before the query engine learned to traverse a `context_assign` under its
   calling context there was no rule that made one usable *where it sits*: the
   only consumers lifted it back out to the caller. Moving the sink from the
   caller (funcptr.c, which passed) into `run` was enough to lose the flow. */

int source();
void sink(int x);

typedef int (*transform_fn)(int);

int passthrough(int v) {
  return v;
}

/* The indirect call and the sink are both here; the pointer comes from main. */
void run(transform_fn f, int v) {
  int r = f(v);
  sink(r);
}

int main() {
  int y = source();
  run(passthrough, y);
}
