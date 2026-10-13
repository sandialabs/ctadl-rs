/* C++ subobject construction (spec 029), POSITIVE (`s1_member_object_default`): a member object
   of a class with a constructor is default-constructed by the enclosing class's (implicit)
   constructor. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; In() : v(source()) {} };
struct Out { In in; int w; };
int main() { Out o; sink(o.in.v); return 0; }
