/* A function pointer installed in a struct field two frames below the frame that owns the
 * struct, then called through that field from a third function. This is libtiff's RGBA decode
 * reduced: TIFFRGBAImageBegin -> PickContigCase stores
 * `img->get = TIFFIsTiled(tif) ? gtTileContig : gtStripContig`, and TIFFRGBAImageGet calls
 * `(*img->get)(img, raster, w, h)`.
 *
 *   - `sink_strips` and `sink_tiles` must both be reached: the source's value is the second
 *     argument of the indirect call, and either target may be installed. `begin` only forwards
 *     the object and makes no indirect call of its own, which is the frame the call-target tag
 *     used to stop at, so the call resolved to nothing and neither sink was reached.
 *
 *   - `sink_clean` must stay silent. Its function is installed in the sibling field `put`,
 *     which is never called, so resolving the call through `get` must not reach it.
 *
 * See `test_cli_query_c_funcptr_stored_two_frames_down` in tests/cli.rs.
 */
int  source(void);
int  is_tiled(void);
void sink_strips(int h);
void sink_tiles(int h);
void sink_clean(int h);

struct img;
typedef int (*get_fn)(struct img *, int);
struct img {
    get_fn get;
    get_fn put;
    int    width;
};

static int get_strips(struct img *im, int h) { sink_strips(h); return 1; }
static int get_tiles(struct img *im, int h)  { sink_tiles(h);  return 1; }
static int put_never(struct img *im, int h)  { sink_clean(h);  return 1; }

/* Installs the targets. No indirect call in this frame or in `begin`. */
static void pick(struct img *im)
{
    im->get = is_tiled() ? get_tiles : get_strips;
    im->put = put_never;
}

static void begin(struct img *im) { pick(im); }

static int get(struct img *im, int h) { return (*im->get)(im, h); }

int main(void)
{
    struct img im;
    int h = source();
    begin(&im);
    return get(&im, h);
}
