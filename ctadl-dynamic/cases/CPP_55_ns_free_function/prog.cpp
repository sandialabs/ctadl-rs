/* C++ namespaces — a free function in a named namespace, called by qualified name. The
   function `ns::id` is an identity transfer; the frontend lowers it under its *qualified*
   IR name `ns::id`, and the qualified call `ns::id(source())` emits a `DirectCall` to that
   name, so taint flows source() -> ns::id's arg -> its return -> sink. The namespace is a
   name-scoping device, not a runtime one, so DFSan sees exactly the flow of a global-scope
   twin; CTADL matches. */
extern "C" int source();
extern "C" void sink(int);

namespace ns {
    int id(int x) { return x; }
}

int main() {
    sink(ns::id(source()));
    return 0;
}
