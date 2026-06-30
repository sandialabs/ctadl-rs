/* Fall-through across a case boundary: case 1 sets the taint and has NO break, so
   control falls into case 2 which carries it onward (deterministic: switch(1) ->
   case 1 -> fall into case 2). At runtime y carries the tainted value. */
extern "C" int source();
extern "C" void sink(int);

int main() {
    int s = source();
    int x = 0;
    int y = 0;
    switch (1) {
        case 1:
            x = s;
            /* no break -- fall through to case 2 */
        case 2:
            y = x;
            break;
        default:
            break;
    }
    sink(y);
    return 0;
}
