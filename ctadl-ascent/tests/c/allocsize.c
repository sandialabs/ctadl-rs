/* A tainted malloc size must not taint the buffer; a byte written through the result must
 * still be followed. See `test_cli_query_c_allocation_size_is_not_contents`. */
int   source_len(void);
char  source_byte(void);
void *malloc(unsigned long n);
void  sink_len(int n);
void  sink_data(char c);
void  sink_hit_local(char c);
void  sink_hit_returned(char c);

/* Writes a tainted byte through malloc's result and returns it. */
static char *make(int n)
{
    char *b = malloc(n);
    b[0] = source_byte();
    return b;
}

int main(void)
{
    int n = source_len();
    char *b = malloc(n);
    sink_len(n);                   /* real: the size itself */
    sink_data(b[0]);               /* must stay silent: nothing was written into b */

    char *c = malloc(16);
    c[1] = source_byte();
    sink_hit_local(c[1]);          /* real: a write through malloc's return, same frame */

    char *d = make(16);
    sink_hit_returned(d[0]);       /* real: a write through malloc's return, in a callee */
    return 0;
}
