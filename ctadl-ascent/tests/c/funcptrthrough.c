/* Call targets crossing an indirect call, both ways. libtiff's codec dispatch is the up case:
 * `(*c->init)(tif)` reaches `InitCCITTFax3`, which installs `tif->tif_encoderow`.
 *
 *   - up:          `init` installs `enc` and is reached only through `setup`'s pointer.
 *   - down:        `run` hands `cb` to `apply`, reached only through `op`.
 *   - down_formal: `run2` hands on its own formal `f` the same way.
 *   - `sink_clean` is in `dec`, installed beside `enc` but never called.
 *
 * See `test_cli_query_c_funcptr_through_indirect_call` in tests/cli.rs.
 */
int  source(void);
void sink_up(int v);
void sink_down(int v);
void sink_down_formal(int v);
void sink_clean(int v);

struct codec {
    void (*enc)(struct codec *, int);
    void (*dec)(struct codec *, int);
};

static void encode(struct codec *c, int v) { sink_up(v); }
static void decode(struct codec *c, int v) { sink_clean(v); }
static void init(struct codec *c) { c->enc = encode; c->dec = decode; }
static void setup(struct codec *c, void (*fn)(struct codec *)) { fn(c); }

static void cb(int v) { sink_down(v); }
static void apply(void (*f)(int), int v) { f(v); }
static void run(void (*op)(void (*)(int), int), int v) { op(cb, v); }

static void cb2(int v) { sink_down_formal(v); }
static void apply2(void (*f)(int), int v) { f(v); }
static void run2(void (*op)(void (*)(int), int), void (*f)(int), int v) { op(f, v); }

int main(void)
{
    struct codec c;
    int v = source();
    setup(&c, init);
    c.enc(&c, v);
    run(apply, v);
    run2(apply2, cb2, v);
    return 0;
}
