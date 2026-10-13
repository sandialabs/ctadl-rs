/* C++ range-based for (spec 026), POSITIVE (`r6_class_elements_method`): a class-typed reference
   loop variable is recorded as an object of its class, so `b.get()` dispatches with the element
   as the receiver. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B {
    int v;
    int get() { return v; }
};

int main() {
    B arr[2];
    arr[0].v = source();
    arr[1].v = 0;
    int s = 0;
    for (B& b : arr)
        s = s + b.get();
    sink(s);
    return 0;
}
