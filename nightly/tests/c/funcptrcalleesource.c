/* D4b (shapes 3 and 4): the taint is created *inside* the resolved callee, and
   leaves it on the callee's own return.

   No summary of `makes_taint` describes this: the value it returns comes from a
   modelled endpoint, which does not exist at index time, so the callee has no
   formal-to-out-formal flow to instantiate. Resolving the call into a summary
   instantiation -- contextual or not -- therefore carries nothing across the
   site. What the flow needs is a *return edge* at the dispatch instruction, and
   `call` is an input relation the fixpoint never extends, so a dynamically
   resolved callee had none.

   Two sinks, one frame apart: `run` consumes the returned value directly
   (shape 3), and `relay` hands it further up to `main` (shape 4). */

int source();
void sink(int x);

typedef int (*fn0)(void);

int makes_taint(void) {
  return source();
}

/* shape 3: the sink sits in the frame holding the indirect call. */
void run(fn0 g) {
  int r = g();
  sink(r);
}

/* shape 4: the value leaves this frame too, and is consumed one frame up. */
int relay(fn0 g) {
  return g();
}

int main() {
  run(makes_taint);
  int r = relay(makes_taint);
  sink(r);
}
