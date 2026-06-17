/* Taint flows directly from source()'s return into sink()'s argument. */
int source();
void sink(int);

int main() {
    int s = source();
    sink(s);
    return 0;
}
