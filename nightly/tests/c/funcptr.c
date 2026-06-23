int source();
void sink(int x);

typedef int (*transform_fn)(int);

// Identity passthrough: taint on the argument flows to the return value.
int passthrough(int v) {
  return v;
}

int main() {
  transform_fn fp = passthrough; // function pointer assignment
  int y = source();
  int z = fp(y); // indirect call through the function pointer
  sink(z);
}
