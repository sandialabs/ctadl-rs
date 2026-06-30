/* C++ explicit `this->member`. The setter writes `this->v` and the getter returns `this->v`;
   CTADL resolves a `this->v` access to the same `this.v` (`@p0.v`) path as the unqualified
   member `v`, so taint flows in through the setter and out through the getter. DFSan observes
   the same member write/read through the method, and CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    void set(int x) { this->v = x; }
    int get() { return this->v; }
};

int main() {
    Box b;
    b.set(source());
    sink(b.get());
    return 0;
}
