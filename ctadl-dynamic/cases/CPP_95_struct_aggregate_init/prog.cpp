/* C++ mirror of case 34. Struct aggregate (brace) initializer: `P p = { s, 0 };` initializes the
   members positionally, so the tainted first element lands on `p.x` -- the member the sink reads.
   The desugaring lives in the shared lowering core, so the C++ frontend produces the same element
   stores the C frontend does; the C++-specific concern is that a brace initializer on a *class
   with a constructor* stays construction (specs 006/010) and is not re-read as an aggregate --
   `P` here has no constructor, so it is a plain aggregate. DFSan observes the flow:
   `s=flow d=flow agree`. Companion negative: CPP_96. Spec 019. */
extern "C" int source();
extern "C" void sink(int);

struct P {
    int x;
    int y;
};

int main() {
    int s = source();
    P p = { s, 0 };
    sink(p.x);
    return 0;
}
