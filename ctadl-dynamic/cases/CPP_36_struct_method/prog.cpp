/* C++ struct (default-public) with instance methods: taint flows IN through a setter and
   OUT through a getter, so it passes through *two* member functions end to end. Confirms
   the method machinery works for `struct` (no `public:` needed) as well as `class`. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    void set(int x) { v = x; }
    int get() { return v; }
};

int main() {
    Box b;
    b.set(source());
    sink(b.get());
    return 0;
}
