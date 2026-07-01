/* C++ constructor overloading (by arity) — POSITIVE (arity 2).
   `Box` declares `Box(int x){v=x;}` and `Box(int x,int y){v=y;}`, keyed as the overload set
   `Box::Box` (lowered `Box::Box#1` / `Box::Box#2`). The construction `Box b(0, source())` has
   TWO explicit arguments, so it resolves to the arity-2 constructor `Box::Box#2`, which writes
   `v = y` from its SECOND argument — so source (the 2nd arg) reaches `v`, while the untainted 0
   (1st arg) is not what `v` receives; `b.get()` reads the tainted `v`. (Because one argument is
   a literal, tree-sitter parses `Box b(0, source())` as an init_declarator with an
   argument_list value, not the most-vexing-parse function declaration.) clang++ selects
   `Box(int,int)`, so DFSan sees the flow (`s=flow d=flow`). */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    Box(int x) { v = x; }
    Box(int x, int y) { v = y; }
    int get() { return v; }
};

int main() {
    Box b(0, source());
    sink(b.get());
    return 0;
}
