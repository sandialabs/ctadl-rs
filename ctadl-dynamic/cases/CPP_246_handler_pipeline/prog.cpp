/* C++ pointer members / new / namespaced bases (spec 042), POSITIVE (`m01_request_pipeline`). A pipeline of `Handler*` in a member array, inside a namespace: `hs[i]->handle(r)` dispatches by CHA. Base names are now resolved from the derived class's namespace (`Logger : Handler` is `http::Handler`), and pointer members are receivers. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace http {
struct Request { int body; int header; Request() : body(0), header(0) {} };
struct Handler { virtual void handle(Request& r) {} virtual ~Handler() {} };
struct Logger : Handler { void handle(Request& r) override { sink(r.body); } };
struct Pipeline {
  Handler* hs[2]; int n;
  Pipeline() : n(0) {}
  void add(Handler* h) { hs[n] = h; n = n + 1; }
  void run(Request& r) { for (int i = 0; i < n; i++) hs[i]->handle(r); }
};
}
int main() { http::Request req; req.body = source(); http::Logger log; http::Pipeline p; p.add(&log); p.run(req); return 0; }
