/* Struct aggregate (brace) initializer: `struct P p = { s, 0 };` initializes the members
   positionally, so the tainted first element lands on `p.x` -- the member the sink reads.
   The frontend desugars the brace initializer element-wise into the field stores the
   programmer could have written (`p.x = s; p.y = 0;`), reusing the existing field-store
   lowering (cf. case 04). DFSan observes the flow, so this is `s=flow d=flow agree`.
   Companion negative: case 35 (the taint lands on the OTHER member). Spec 019. */
int source();
void sink(int);

struct P {
    int x;
    int y;
};

int main() {
    int s = source();
    struct P p = { s, 0 };
    sink(p.x);
    return 0;
}
