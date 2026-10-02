/* One indirect call site that resolves to two targets: the function pointer is stored and
 * called in the same frame, through a ternary. The query has to enter BOTH targets' formals.
 * It used to keep one callee per call site, so only the last target it saw was entered and
 * the sink in the other was never reached.
 *
 * See `test_cli_query_c_funcptr_with_two_targets` in tests/cli.rs.
 */
int  source(void);
int  pick(void);
void sink_a(int h);
void sink_b(int h);

struct img;
typedef int (*get_fn)(struct img *, int);
struct img {
    get_fn get;
};

static int get_a(struct img *im, int h) { sink_a(h); return 1; }
static int get_b(struct img *im, int h) { sink_b(h); return 1; }

int main(void)
{
    struct img im;
    int h = source();
    im.get = pick() ? get_a : get_b;
    return im.get(&im, h);
}
