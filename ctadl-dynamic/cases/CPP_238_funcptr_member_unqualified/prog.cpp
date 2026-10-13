/* C++ returned objects / function-pointer members (spec 041), POSITIVE (`p08_callback_member`). `cb(x)` inside a method, with `cb` a function-pointer member, used to be a direct call to a global `cb`: `member_name` did not unwrap `void (*cb)(int)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Btn { void (*cb)(int); void click(int x) { cb(x); } };
void handler(int x) { sink(x); }
int main() { Btn b; b.cb = handler; b.click(source()); return 0; }
