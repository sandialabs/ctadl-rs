/* C++ class-typed parameter as a method receiver (spec 020), NEGATIVE — field sensitivity
   through a parameter receiver. `fill` taints member `v` through a *reference* parameter (so
   the write really does reach the caller's object, as CPP_100 shows), but `main` sinks the
   DISTINCT member `w`. Dispatching on a parameter must keep the receiver's members apart
   exactly as dispatching on a local does: `@p0.v` and `@p0.w` are separate access paths, so
   the taint never crosses. If registering a parameter's class had smeared taint across the
   whole receiver object, this would report a spurious flow. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Box {
    int v;
    int w;
    void setV(int x) { v = x; }
    int getW() { return w; }
};

void fill(Box& b) { b.setV(source()); }   /* taints the receiver's `v` only */

int main() {
    Box a;
    a.v = 0;
    a.w = 0;
    fill(a);            /* writes back into a.v */
    sink(a.getW());     /* reads the DISTINCT member a.w (=0) -- no flow */
    return 0;
}
