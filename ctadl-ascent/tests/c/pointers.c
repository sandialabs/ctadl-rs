/* The C pointer model: every sink_hit_* is reached, sink_clean_* is not.
 * See `test_cli_query_c_pointer_model`. */
char *source_buf(void);           /* the bytes it returns are tainted */
char  source_byte(void);
int   source_int(void);
void  use(char *p);
void  sink_hit_rest(char *b);     /* sinks the bytes at b */
void  sink_hit_moved(char *p);
void  sink_hit_star_index(char c);
void  sink_hit_index_star(char c);
void  sink_hit_copy(char c);
void  sink_hit_loop(char c);
void  sink_hit_out(int v);
void  sink_hit_offset(char *p);
void  sink_clean_caller(char *p);

/* Moves its own copy of the pointer; the caller's is unchanged. */
static void advance(char *in, int skew) { in += skew; use(in); }
static void advance_sink(char *in, int skew) { in += skew; sink_hit_moved(in); }
static void put_star(char *p) { *p = source_byte(); }
static void put_index(char *p) { p[0] = source_byte(); }
static void copy(char *out, char *in, int n) { while (n--) *out++ = *in++; }
static void get(int *out) { *out = source_int(); }

int main(void)
{
    /* Writing one byte does not clean the rest of the buffer. */
    char *a = source_buf();
    *a = 7;
    sink_hit_rest(a);

    char buf[8];
    int skew = source_int();
    advance(buf, skew);
    sink_clean_caller(buf);
    advance_sink(buf, skew);

    /* `*p` and `p[0]` are one location. */
    char m[4], n[4];
    put_star(m);  sink_hit_star_index(m[0]);
    put_index(n); sink_hit_index_star(*n);

    /* A write through a copy of a pointer. */
    char s[4];
    char *q = s;
    *q = source_byte();
    sink_hit_copy(s[0]);

    char dst[8];
    copy(dst, source_buf(), 8);
    sink_hit_loop(dst[0]);

    int w = 0;
    get(&w);
    sink_hit_out(w);

    /* A tainted offset makes a tainted pointer. */
    char t[64];
    int off = source_int();
    sink_hit_offset(t + off);
    return 0;
}
