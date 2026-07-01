/* C++ constructor negative — field sensitivity. The constructor taints member `a` from its
   argument (`a = x`) and sets `b` to a constant (`b = 0`). Construction `Box bx(source())`
   genuinely taints `bx.a`, but the sink reads `bx.b`, a *distinct* member that was set to a
   constant. CTADL models the constructor's writes per member (`this.a := x`, `this.b := 0`),
   so `bx.b` carries no taint (s=none); DFSan agrees (b's shadow stays clean, d=none). If the
   constructor smeared the argument across all members, this would risk a spurious flow. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int a;
    int b;
    Box(int x) { a = x; b = 0; }
    int getb() { return b; }
};

int main() {
    Box bx(source());   /* taints bx.a -- real taint enters the program */
    sink(bx.getb());    /* reads bx.b -- a distinct, untainted member */
    return 0;
}
