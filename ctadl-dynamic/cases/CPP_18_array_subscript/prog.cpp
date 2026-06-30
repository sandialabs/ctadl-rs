/* Taint stored into and read from an array element. The CTADL tree-sitter C
   frontend currently cannot ingest the `int a[3];` array declarator (ERR 78), so
   this is allowlisted as a known frontend gap. DFSan compiles and runs it fine,
   so the moment the frontend learns arrays, the harness will compare results. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int a[3];
    a[1] = source();
    sink(a[1]);
    return 0;
}
