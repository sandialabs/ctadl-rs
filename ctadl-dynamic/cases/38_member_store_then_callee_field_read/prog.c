/* F4 (OPEN, known gap): an aggregate stored into a member (`o.in = i`) and then passed to a
   callee that reads a field of it (`get(o.in)` reads `.v`). The taint must live at `o.in.v`,
   but that composed path is admissible only if the program spells it: codegen composes field
   paths along a load chain within one block (`cap_path`), and here the `.v` read is inside the
   callee. The materialized-paths gate drops it. d=flow, s=none. */
int source();
void sink(int);

struct In { int v; };
struct Out { struct In in; };

int get(struct In p) { return p.v; }

int main() {
    struct In i;
    struct Out o;
    i.v = source();
    o.in = i;
    sink(get(o.in));
    return 0;
}
