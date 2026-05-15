int source();
void sink(int);

void transfer(int *a, int b) {
  a[1] = b;
}

int main() {
  int s;
  int x[3];
  s = source();
  transfer(&x[1], s);
  sink(x[2]);
}
